// Owns the tests of the naming request behind `termal_name_source_root`
// (`AppState::name_engram_source_root`) and of the host line it and a bind
// leave for the agent's next prompt: the refusals, the generation a name gets,
// the seal a clear leaves on the running turn, the full list's reclaim within
// its budget, and when the line counts as delivered. Does not own the
// validation, the end rules or the basis taken on a named root
// (src/tests/engram_source_roots.rs), or turns measured in a named root
// (src/tests/engram_turn_observations.rs, whose `ClaimedRoot` fixture this
// child module uses). New module.
use super::*;

/// The claimed root's project given the store identity a real Engram
/// reports, which names a source root in it.
fn claimed_root_store(claimed: &ClaimedRoot) -> EngramAuthorityStoreKey {
    let store = EngramAuthorityStoreKey {
        database_path: claimed.root.join("engram.db"),
        project_id: "github.com/example/source-root".to_owned(),
    };
    let mut inner = claimed.state.inner.lock().expect("state mutex poisoned");
    for project in &mut inner.projects {
        if let Some(engram) = project.engram.as_mut() {
            engram.authority_store_key = Some(store.clone());
        }
    }
    store
}

/// The held-claims list of the claimed root's session: the claim it is bound
/// to by `ClaimedRoot::new`.
fn claimed_root_held(label: &str) -> EngramHeldClaims {
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(),
            short_ref: format!("w-{label}"),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            focused: true,
            control_binding: Some(binding),
        }],
        omitted: 0,
    }
}

/// One naming request of the claimed root's session for `w-{label}`, with
/// Engram answering its held-claims reads with `reads` only (the last
/// repeats), whatever earlier requests were answered with.
fn name_root(
    claimed: &ClaimedRoot,
    label: &str,
    path: Option<&FsPath>,
    reads: Vec<EngramHeldClaims>,
) -> std::result::Result<EngramSourceRootResponse, ApiError> {
    claimed_root_store(claimed);
    claimed.transport.replace_held_claims(reads.into_iter().map(Ok));
    claimed.state.name_engram_source_root(
        &claimed.session_id,
        EngramSourceRootRequest {
            work: format!("w-{label}"),
            path: path.map(|path| Some(path.to_string_lossy().into_owned())),
        },
    )
}

/// A complete held-claims list with no claim on it.
fn no_held_claims() -> EngramHeldClaims {
    EngramHeldClaims {
        items: Vec::new(),
        omitted: 0,
    }
}

fn pending_line(claimed: &ClaimedRoot) -> Option<String> {
    claimed.record(|record| record.engram.pending_source_root_line.clone())
}

#[test]
fn a_name_is_refused_without_the_live_claim_to_a_delegated_session_and_on_a_share() {
    let label = "naming-refusals";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);

    let other_work = EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: "work-other".to_owned(),
            short_ref: "w-other".to_owned(),
            claim_id: "claim-other".to_owned(),
            ..Default::default()
        }],
        omitted: 0,
    };
    let error = name_root(&claimed, label, Some(&worktree), vec![other_work.clone()])
        .expect_err("no live claim on the work");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("holds no live claim"), "{}", error.message);
    assert!(!error.message.contains("left out"), "{}", error.message);

    let truncated = EngramHeldClaims {
        omitted: 3,
        ..other_work
    };
    let error = name_root(&claimed, label, Some(&worktree), vec![truncated])
        .expect_err("the claim is not on the list Engram gave");
    assert!(
        error.message.contains("3 more were left out of its list"),
        "the refusal says the list was cut: {}",
        error.message
    );

    let mut no_claim_id = claimed_root_held(label);
    no_claim_id.items[0].claim_id.clear();
    let error = name_root(&claimed, label, Some(&worktree), vec![no_claim_id])
        .expect_err("an Engram without claim ids cannot scope a name");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);

    // Named by its id, since a row without a short reference cannot be
    // found by one.
    let mut no_short_ref = claimed_root_held(label);
    no_short_ref.items[0].short_ref.clear();
    let work_id = no_short_ref.items[0].work_id.clone();
    claimed.transport.replace_held_claims([Ok(no_short_ref)]);
    let error = claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: work_id,
                path: Some(Some(worktree.to_string_lossy().into_owned())),
            },
        )
        .expect_err("an entry without a short reference cannot match its evaluations");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert!(error.message.contains("short reference"), "{}", error.message);

    let error = name_root(
        &claimed,
        label,
        Some(FsPath::new("//server/share/wt")),
        vec![claimed_root_held(label)],
    )
    .expect_err("a share is refused");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("network share"), "{}", error.message);
    // So is a verbatim alias of the network redirector, before anything
    // resolves it.
    let error = name_root(
        &claimed,
        label,
        Some(FsPath::new(r"\\?\GLOBALROOT\Device\Mup\server\share\wt")),
        vec![claimed_root_held(label)],
    )
    .expect_err("a redirector alias is refused");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("network share"), "{}", error.message);

    let error = name_root(
        &claimed,
        label,
        Some(FsPath::new("   ")),
        vec![claimed_root_held(label)],
    )
    .expect_err("a blank path is no clear");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("`path` is blank"), "{}", error.message);

    let error = name_root(
        &claimed,
        label,
        Some(FsPath::new(&"x".repeat(MAX_DELEGATION_CWD_CHARS + 1))),
        vec![claimed_root_held(label)],
    )
    .expect_err("an overlong path is refused before it is checked");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("at most"), "{}", error.message);

    // A delegated session by its own link, with no delegation row left.
    claimed.record(|record| {
        record.session.parent_delegation_id = Some("delegation-gone".to_owned());
    });
    let error = name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
        .expect_err("a session linked to a parent names nothing");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("only a root session"), "{}", error.message);
    claimed.record(|record| record.session.parent_delegation_id = None);

    claimed
        .state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .delegations
        .push(DelegationRecord {
            id: "delegation-naming".to_owned(),
            parent_session_id: "session-parent".to_owned(),
            child_session_id: claimed.session_id.clone(),
            mode: DelegationMode::Worker,
            status: DelegationStatus::Running,
            title: "Worker".to_owned(),
            prompt: "Work.".to_owned(),
            cwd: claimed.root.to_string_lossy().into_owned(),
            agent: Agent::Codex,
            model: None,
            write_policy: DelegationWritePolicy::ReadOnly,
            created_at: stamp_now(),
            started_at: None,
            completed_at: None,
            result: None,
            submitted_review_result: None,
            post_submission_transport_error: None,
            review_result_recovery_probe_attempt: None,
            review_result_recovery_error: None,
            review_result_schema_version: None,
            queued_followup_prompt_id: None,
            review_result_submission_attempt: 0,
            acceptance_evaluation: None,
            attempt: DelegationAttemptState::default(),
        });
    let error = name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
        .expect_err("a delegated session names nothing");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("only a root session"), "{}", error.message);
    assert!(
        claimed
            .state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_work_source_roots
            .is_empty(),
        "no refusal left an entry"
    );
}

#[test]
fn the_same_root_named_again_keeps_its_generation_and_a_name_after_a_clear_gets_a_new_one() {
    let label = "naming-generations";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let held = || vec![claimed_root_held(label)];

    let first = name_root(&claimed, label, Some(&worktree), held()).expect("the first name");
    let store = claimed_root_store(&claimed);
    let requested_on = {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        AcceptanceEvaluationSourceRoot::from_entry(&inner.engram_work_source_roots[0])
    };
    let again = name_root(&claimed, label, Some(&worktree), held()).expect("the same name again");
    assert_eq!(again.generation, first.generation, "nothing changed");
    assert_eq!(again.sealed, None);
    {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        assert!(
            requested_on.still_named(&inner.engram_work_source_roots, &store),
            "an evaluation requested on the root is not refused for a repeated name"
        );
    }

    let cleared = name_root(&claimed, label, None, held()).expect("a clear");
    assert_eq!(cleared.generation, 0);
    assert_eq!(cleared.root, None);
    assert_eq!(
        cleared.sealed, None,
        "no turn runs in the old root, so the clear reports no seal"
    );
    let renamed = name_root(&claimed, label, Some(&worktree), held()).expect("named again");
    assert!(
        renamed.generation > first.generation,
        "a name after a clear is a new one: {} after {}",
        renamed.generation,
        first.generation
    );
    let inner = claimed.state.inner.lock().expect("state mutex poisoned");
    assert!(
        !requested_on.still_named(&inner.engram_work_source_roots, &store),
        "the evaluation requested before the clear is refused"
    );
}

#[test]
fn a_clear_during_a_turn_seals_the_root_that_turn_is_measured_in() {
    let label = "naming-clear-seals";
    let (claimed, worktree, _runtime_token) = named_root_turn(label, true);
    assert!(
        claimed.record(|record| record.engram.active_turn_source_root.is_some()),
        "the turn was admitted with the named root"
    );

    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("a clear");

    let revision = content_revision_of(&worktree);
    assert_eq!(
        cleared.sealed.and_then(|sealed| sealed.source_revision),
        Some(revision.clone())
    );
    assert_eq!(
        claimed.record(|record| {
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .and_then(|turn_root| turn_root.sealed_revision.clone())
        }),
        Some(revision),
        "the running turn keeps its root, sealed at the clear"
    );
    assert!(
        pending_line(&claimed).is_some_and(|line| line.contains("no worktree named")),
        "the next prompt says the workdir measures the work again"
    );
}

#[test]
fn a_clear_seals_no_turn_of_another_claim_measured_in_the_same_worktree() {
    // Two works may name one worktree. A clear of one work's name leaves a
    // turn of the other work's claim in that tree unsealed: that work still
    // names the tree, so a seal would hide its later edits there once the
    // tree is gone.
    let label = "naming-clear-other-claim";
    let (claimed, _worktree, _runtime_token) = named_root_turn(label, true);
    claimed.record(|record| {
        record
            .engram
            .active_turn_source_root
            .as_mut()
            .expect("the turn was admitted with the named root")
            .claim_id = "claim-of-another-work".to_owned();
    });

    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("a clear");

    assert_eq!(cleared.sealed, None, "no turn took a seal");
    assert_eq!(
        claimed.record(|record| {
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .map(|turn_root| turn_root.sealed_revision.clone())
        }),
        Some(None),
        "the other claim's turn keeps its root, unsealed"
    );
}

/// Changes the Engram settings of the claimed root's project.
fn change_engram_settings(state: &AppState, change: impl Fn(&mut EngramProjectSettings)) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    for project in &mut inner.projects {
        if let Some(engram) = project.engram.as_mut() {
            change(engram);
        }
    }
}

#[test]
fn a_name_is_refused_when_engram_control_is_off_or_the_project_changes_meanwhile() {
    let label = "naming-project-fence";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let nothing_named = |claimed: &ClaimedRoot, case: &str| {
        assert!(
            claimed
                .state
                .inner
                .lock()
                .expect("state mutex poisoned")
                .engram_work_source_roots
                .is_empty(),
            "{case}: no entry"
        );
        assert_eq!(pending_line(claimed), None, "{case}: no host line");
    };

    // A name counts only for mediated turns: with Engram control off there
    // is nothing to name.
    change_engram_settings(&claimed.state, |engram| engram.turn_gated_control = false);
    let error = name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
        .expect_err("control is off");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("not enabled"), "{}", error.message);
    nothing_named(&claimed, "control off");
    change_engram_settings(&claimed.state, |engram| engram.turn_gated_control = true);

    // The project changes while the name is being checked: another store,
    // or control turned off. Nothing is named for a store nobody could
    // clear it in.
    let changes: [(&str, fn(&mut EngramProjectSettings)); 2] = [
        ("another store", |engram| {
            if let Some(store) = engram.authority_store_key.as_mut() {
                store.project_id = "github.com/example/another".to_owned();
            }
        }),
        ("control turned off", |engram| engram.turn_gated_control = false),
    ];
    for (case, change) in changes {
        let state = claimed.state.clone();
        TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || change_engram_settings(&state, change)));
        });
        let error = name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
            .expect_err(case);
        assert!(
            TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| hook.borrow().is_none()),
            "{case}: the change landed inside the call"
        );
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}");
        assert!(
            error.message.contains("changed while the root was being named"),
            "{case}: {}",
            error.message
        );
        nothing_named(&claimed, case);
        change_engram_settings(&claimed.state, |engram| engram.turn_gated_control = true);
    }
}

/// The claimed root's turn record: its grant, and the revision sealed on its
/// root, if any.
fn turn_seal(claimed: &ClaimedRoot) -> (Option<String>, Option<String>) {
    claimed.record(|record| {
        (
            record.engram.active_grant_id.clone(),
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .and_then(|turn_root| turn_root.sealed_revision.clone()),
        )
    })
}

#[test]
fn a_clear_after_the_turn_finished_seals_nothing() {
    // A finished turn keeps its root on the record until the next grant
    // begins, but a clear after it has no running turn to seal.
    let label = "naming-clear-after-finish";
    let (claimed, _worktree, runtime_token) = named_root_turn(label, true);
    let _ = finish_claimed_turn(&claimed, &runtime_token);
    assert!(
        claimed.record(|record| record.engram.active_turn_source_root.is_some()),
        "the finished turn's root is still on the record"
    );

    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("a clear");

    assert_eq!(cleared.sealed, None, "no running turn took a seal");
    assert_eq!(turn_seal(&claimed), (None, None), "the finished turn stays unsealed");
}

#[test]
fn a_clear_seals_no_turn_admitted_after_the_call_began() {
    // A turn admitted while the name is being given measures what it finds:
    // the seal's capture may predate that turn's own start.
    let label = "naming-clear-later-turn";
    let (claimed, _worktree, _runtime_token) = named_root_turn(label, true);
    let (state, session_id) = (claimed.state.clone(), claimed.session_id.clone());
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            // The running turn ends and a later one begins, with the same
            // root, after the call's first read.
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session_id).expect("the session exists");
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .engram
                .active_grant_id = Some("grant-of-a-later-turn".to_owned());
        }));
    });

    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("a clear");

    assert!(
        TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| hook.borrow().is_none()),
        "the later turn began inside the call"
    );
    assert_eq!(cleared.sealed, None, "no turn took a seal");
    assert_eq!(
        turn_seal(&claimed),
        (Some("grant-of-a-later-turn".to_owned()), None),
        "the later turn keeps its root unsealed"
    );
}

#[cfg(windows)]
#[test]
fn a_git_bash_drive_spelling_names_the_worktree() {
    // Git Bash's `pwd` prints `/c/…` for `C:/…`; read as written it would be
    // joined to the workdir.
    let label = "naming-msys-drive";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    let windows = engram_source_root_display(&canonical.to_string_lossy()).replace('\\', "/");
    let (drive, rest) = windows.split_once(':').expect("a drive path");
    let msys = format!("/{}{rest}", drive.to_ascii_lowercase());

    let named = name_root(
        &claimed,
        label,
        Some(FsPath::new(&msys)),
        vec![claimed_root_held(label)],
    )
    .unwrap_or_else(|error| panic!("{msys} should name the worktree: {}", error.message));

    assert_eq!(
        named.root.as_deref().map(|root| engram_exact_path_key(FsPath::new(root))),
        Some(engram_exact_path_key(&canonical)),
        "{msys} names the worktree itself"
    );
}

/// Another session of the claimed root's project.
fn other_session(claimed: &ClaimedRoot) -> String {
    let project_id = claimed.record(|record| {
        record
            .session
            .project_id
            .clone()
            .expect("the claimed root has a project")
    });
    create_test_project_session(&claimed.state, Agent::Codex, &project_id, &claimed.root)
}

/// A full list, every entry named by `named_by` for a claim that has ended.
fn fill_list(claimed: &ClaimedRoot, store: &EngramAuthorityStoreKey, named_by: &str) {
    claimed
        .state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .engram_work_source_roots = (0..ENGRAM_WORK_SOURCE_ROOT_LIMIT)
        .map(|index| EngramWorkSourceRoot {
            store: store.clone(),
            work_id: format!("work-ended-{index}"),
            short_ref: format!("w-ended-{index}"),
            claim_id: format!("claim-ended-{index}"),
            claim_fence: 1,
            root: claimed.root.to_string_lossy().into_owned(),
            common_dir_key: "unused".to_owned(),
            named_by_session: named_by.to_owned(),
            named_at: "2026-09-27T00:00:00.000Z".to_owned(),
            generation: 1,
        })
        .collect();
}

#[test]
fn a_full_list_is_reclaimed_from_the_ended_claims_of_other_sessions() {
    let label = "naming-reclaim";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let store = claimed_root_store(&claimed);
    let other = other_session(&claimed);
    fill_list(&claimed, &store, &other);

    // The caller's own read, then the other session's: it holds nothing now.
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label), no_held_claims()],
    )
    .expect("the ended claims' entries make room");

    let inner = claimed.state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.engram_work_source_roots.len(), 1, "only the new name is left");
    assert_eq!(
        inner.engram_work_source_roots[0].root,
        named.root.expect("a root was named")
    );
}

#[test]
fn a_list_the_callers_own_ended_entries_free_reads_no_other_session() {
    // One entry of the full list is the caller's own, for a claim it no
    // longer holds: its own read makes room, so no other session is read and
    // the other session's entries stay.
    let label = "naming-reclaim-own";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let store = claimed_root_store(&claimed);
    let other = other_session(&claimed);
    fill_list(&claimed, &store, &other);
    claimed.state.inner.lock().expect("state mutex poisoned").engram_work_source_roots[0]
        .named_by_session = claimed.session_id.clone();

    // Were the other session read, this repeated answer would end its
    // entries: it holds none of their claims.
    name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
        .expect("the caller's own ended entry makes room");

    let inner = claimed.state.inner.lock().expect("state mutex poisoned");
    assert_eq!(
        inner.engram_work_source_roots.len(),
        ENGRAM_WORK_SOURCE_ROOT_LIMIT,
        "the other session's entries stay, and the new name took the freed place"
    );
    assert_eq!(
        inner
            .engram_work_source_roots
            .iter()
            .filter(|entry| entry.named_by_session == other)
            .count(),
        ENGRAM_WORK_SOURCE_ROOT_LIMIT - 1
    );
}

#[test]
fn a_reclaim_read_not_started_within_the_budget_is_skipped() {
    let label = "naming-reclaim-budget";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let store = claimed_root_store(&claimed);
    let other = other_session(&claimed);
    fill_list(&claimed, &store, &other);
    claimed.transport.replace_held_claims([Ok(no_held_claims())]);

    assert!(
        claimed
            .state
            .engram_source_root_held_reads_of_other_sessions(
                &claimed.session_id,
                std::time::Instant::now(),
            )
            .is_empty(),
        "a spent budget reads nothing, so it ends nothing"
    );
    let reads = claimed.state.engram_source_root_held_reads_of_other_sessions(
        &claimed.session_id,
        std::time::Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(
        reads.iter().map(|(named_by, _, _)| named_by.as_str()).collect::<Vec<_>>(),
        [other.as_str()],
        "one read per other naming session, never the caller's"
    );
}

#[test]
fn a_bound_claim_tells_the_agent_where_its_turns_are_measured_in_its_first_prompt() {
    let label = "naming-bind-line";
    let grant_id = "turn-observation-naming-bind-line-grant";
    let claimed = ClaimedRoot::new(
        label,
        vec![
            bind_reply("turn-observation-naming-bind-line-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );

    deliver_turn_dispatch(&claimed.state, claimed.dispatch())
        .expect("the begun root turn should reach the runtime");

    let prompt = received_prompt(&claimed);
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    assert!(
        prompt.contains(&format!("Engram source basis for work {}:", binding.work_id))
            && prompt.contains("no worktree named"),
        "the bind's line, naming the work by its id with no name given: {prompt}"
    );
    assert_eq!(pending_line(&claimed), None, "a delivered line is not repeated");
}

#[test]
fn a_line_set_after_the_prompt_was_built_waits_for_the_next_prompt() {
    let label = "naming-late-line";
    let grant_id = "turn-observation-naming-late-line-grant";
    let claimed = ClaimedRoot::new(
        label,
        vec![
            bind_reply("turn-observation-naming-late-line-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    let dispatch = claimed.dispatch();
    // Set between the prompt's build and its delivery, as a rebind during
    // the begin's recovery sets one.
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] a later line".to_owned());
    });

    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");

    let prompt = received_prompt(&claimed);
    assert!(!prompt.contains("a later line"), "{prompt}");
    assert_eq!(
        pending_line(&claimed).as_deref(),
        Some("[TermAl] a later line"),
        "the later line is kept for the next prompt, not lost"
    );
}

#[test]
fn a_line_the_prompt_carried_given_again_after_its_build_stays_the_last_line() {
    // The prompt carries a name; before the runtime accepts it the name is
    // cleared and given again. The acceptance must not take the name given
    // again, or the clear would end the next prompt's lines.
    let label = "naming-line-again";
    let grant_id = "turn-observation-naming-line-again-grant";
    let claimed = ClaimedRoot::new(
        label,
        vec![
            bind_reply("turn-observation-naming-line-again-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] named".to_owned());
    });
    let dispatch = claimed.dispatch();
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] cleared".to_owned());
        record
            .engram
            .set_pending_source_root_line("[TermAl] named".to_owned());
    });

    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");

    let prompt = received_prompt(&claimed);
    assert!(prompt.contains("[TermAl] named"), "{prompt}");
    assert!(!prompt.contains("[TermAl] cleared"), "{prompt}");
    assert_eq!(
        pending_line(&claimed).as_deref(),
        Some("[TermAl] cleared\n[TermAl] named"),
        "the next prompt tells the clear and ends on the name given again"
    );
}

#[test]
fn a_session_that_engram_disables_drops_its_waiting_lines() {
    // Once Engram disables the session, its turns are measured nowhere: a
    // line waiting for the next prompt would tell of measurements that no
    // longer happen. A failure that only delays the session keeps it.
    let label = "naming-disabled-drops-lines";
    let claimed = ClaimedRoot::new(label, Vec::new());
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] a waiting line".to_owned());
        record.engram.source_root_line_delivery =
            Some(("[TermAl] a waiting line".to_owned(), 7));
    });

    claimed.state.record_engram_transport_failure(
        &claimed.session_id,
        &EngramTransportError::deadline("control deadline"),
    );
    assert_eq!(
        pending_line(&claimed).as_deref(),
        Some("[TermAl] a waiting line"),
        "a failure that does not disable the session keeps the line"
    );

    claimed.state.record_engram_transport_failure(
        &claimed.session_id,
        &EngramTransportError::protocol("unparsable control response"),
    );
    assert!(
        claimed.record(|record| record.engram.disabled_reason.is_some()),
        "the protocol failure disabled the session"
    );
    assert_eq!(pending_line(&claimed), None, "the waiting line is dropped");
    assert_eq!(
        claimed.record(|record| record.engram.source_root_line_delivery.clone()),
        None,
        "and so is its delivery in flight"
    );
}

#[test]
fn a_delivered_line_is_cleared_on_a_turn_that_begins_no_grant() {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-source-root-line-no-grant");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-source-root-line-no-grant-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "No Engram");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let set_line = |line: &str| {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).expect("session");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .set_pending_source_root_line(line.to_owned());
    };
    set_line("[TermAl] a line for a turn without a grant");

    let dispatch = match state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "Hello.".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the session should dispatch")
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("an idle session should dispatch"),
    };
    deliver_turn_dispatch(&state, dispatch).expect("the prompt should reach the runtime");

    match receive(&runtime_rx, "runtime should receive the prompt") {
        CodexRuntimeCommand::Prompt { command, .. } => {
            assert!(command.prompt.contains("a line for a turn without a grant"))
        }
        _ => panic!("expected a prompt"),
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&session_id).expect("session");
    assert_eq!(
        inner.sessions[index].engram.pending_source_root_line, None,
        "delivered once, not repeated on every prompt"
    );
}

#[test]
fn a_clear_during_the_opening_capture_seals_the_root_that_turn_began_with() {
    let label = "naming-seal-during-capture";
    let grant_id = "turn-observation-naming-seal-during-capture-grant";
    let claimed = ClaimedRoot::new(
        label,
        vec![
            bind_reply("turn-observation-naming-seal-during-capture-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
        .expect("the root is named before the turn");
    let (state, session_id, work) = (
        claimed.state.clone(),
        claimed.session_id.clone(),
        format!("w-{label}"),
    );
    // The clear lands after the turn's root is kept and before its opening
    // capture: the window a seal must not miss.
    TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            state
                .name_engram_source_root(&session_id, EngramSourceRootRequest { work, path: None })
                .expect("a clear during the opening capture");
        }));
    });

    deliver_turn_dispatch(&claimed.state, claimed.dispatch())
        .expect("the begun root turn should reach the runtime");
    let _ = received_prompt(&claimed);

    assert!(
        TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| hook.borrow().is_none()),
        "the clear ran inside the window"
    );
    assert_eq!(
        claimed.record(|record| {
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .and_then(|turn_root| turn_root.sealed_revision.clone())
        }),
        Some(content_revision_of(&worktree)),
        "the turn keeps the root it began with, sealed at the clear"
    );
}

#[test]
fn a_watcher_event_in_a_named_root_beside_the_workdir_counts_while_the_grant_is_held() {
    let label = "naming-watcher-sibling";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    // The session works in a directory beside the named root, not above it.
    let workdir = claimed.root.join("beside");
    fs::create_dir_all(&workdir).expect("the workdir exists");
    claimed.record(|record| {
        record.session.workdir = engram_source_root_display(
            &fs::canonicalize(&workdir)
                .expect("the workdir canonicalizes")
                .to_string_lossy(),
        );
        record.active_turn_start_message_count = Some(record.session.messages.len());
        record.engram.active_grant_id = Some("grant-watched".to_owned());
        record.engram.active_turn_source_root = Some(EngramTurnSourceRoot {
            root: canonical.to_string_lossy().into_owned(),
            common_dir_key: "unused".to_owned(),
            short_ref: format!("w-{label}"),
            claim_id: "claim-watched".to_owned(),
            generation: 0,
            sealed_revision: None,
        });
    });
    let in_root = engram_source_root_display(&canonical.join("README.md").to_string_lossy());
    let elsewhere = engram_source_root_display(
        &fs::canonicalize(&claimed.root)
            .expect("the root canonicalizes")
            .join("elsewhere.txt")
            .to_string_lossy(),
    );
    let events = |paths: &[&str]| {
        paths
            .iter()
            .map(|path| WorkspaceFileChangeEvent {
                path: (*path).to_owned(),
                kind: WorkspaceFileChangeKind::Modified,
                root_path: None,
                session_id: None,
                mtime_ms: None,
                size_bytes: None,
            })
            .collect::<Vec<_>>()
    };

    claimed
        .state
        .record_active_turn_file_changes(&events(&[&in_root, &elsewhere]));
    assert_eq!(
        claimed.record(|record| {
            record
                .active_turn_file_changes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        }),
        [in_root.clone()],
        "the turn's root counts; a path outside it and the workdir does not"
    );

    claimed.record(|record| {
        record.active_turn_file_changes.clear();
        record.engram.active_grant_id = None;
    });
    claimed
        .state
        .record_active_turn_file_changes(&events(&[&in_root]));
    assert!(
        claimed.record(|record| record.active_turn_file_changes.is_empty()),
        "a root kept from an earlier grant counts for nothing"
    );
}

#[test]
fn the_naming_route_answers_with_the_handlers_statuses() {
    let label = "naming-route";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    claimed_root_store(&claimed);
    claimed
        .transport
        .replace_held_claims([Ok(claimed_root_held(label))]);
    let app = app_router(claimed.state.clone());
    let post = |session: &str, body: Value| {
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session}/engram-source-root"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("the body serializes")))
            .expect("the request builds")
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a test runtime");

    let (status, named): (StatusCode, Value) = runtime.block_on(request_json(
        &app,
        post(
            &claimed.session_id,
            json!({ "work": format!("w-{label}"), "path": worktree.to_string_lossy() }),
        ),
    ));
    assert_eq!(status, StatusCode::OK, "{named:#}");
    assert_eq!(named["workRef"], format!("w-{label}"));
    assert!(named["root"].is_string(), "{named:#}");

    let unknown = runtime.block_on(request_response(
        &app,
        post(&claimed.session_id, json!({ "work": "w-x", "bogus": 1 })),
    ));
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let missing = runtime.block_on(request_response(
        &app,
        post("session-missing", json!({ "work": "w-x" })),
    ));
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[test]
fn a_watcher_event_in_the_named_root_counts_when_another_sessions_scope_carries_it() {
    // The named root lies under another session's workdir, so the watcher
    // routes its events to that session's scope and marks the unscoped copy
    // as a duplicate; the turn measured in the root still counts them.
    let label = "naming-watcher-scoped";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    let project_root = fs::canonicalize(&claimed.root).expect("the root canonicalizes");
    let other = other_session(&claimed);
    let workdir = claimed.root.join("beside");
    fs::create_dir_all(&workdir).expect("the workdir exists");
    claimed.record(|record| {
        record.session.workdir = engram_source_root_display(
            &fs::canonicalize(&workdir)
                .expect("the workdir canonicalizes")
                .to_string_lossy(),
        );
        record.active_turn_start_message_count = Some(record.session.messages.len());
        record.engram.active_grant_id = Some("grant-watched".to_owned());
        record.engram.active_turn_source_root = Some(EngramTurnSourceRoot {
            root: canonical.to_string_lossy().into_owned(),
            common_dir_key: "unused".to_owned(),
            short_ref: format!("w-{label}"),
            claim_id: "claim-watched".to_owned(),
            generation: 0,
            sealed_revision: None,
        });
    });
    let scope_root =
        PathBuf::from(engram_source_root_display(&project_root.to_string_lossy()));
    // The other session works in the same tree and has a turn running too.
    {
        let mut inner = claimed.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&other)
            .expect("the other session exists");
        let record = &mut inner.sessions[index];
        record.session.workdir = scope_root.to_string_lossy().into_owned();
        record.active_turn_start_message_count = Some(record.session.messages.len());
    }
    let scopes = [
        WorkspaceFileWatchScope {
            root_path: scope_root.clone(),
            session_id: None,
        },
        WorkspaceFileWatchScope {
            root_path: scope_root,
            session_id: Some(other.clone()),
        },
    ];
    let edited = PathBuf::from(engram_source_root_display(
        &canonical.join("README.md").to_string_lossy(),
    ));
    let events =
        workspace_file_changes_from_path(&edited, WorkspaceFileChangeKind::Modified, &scopes);
    assert!(
        events
            .iter()
            .all(|event| event.session_id.as_deref() != Some(claimed.session_id.as_str())),
        "no copy is routed to the session measured in the root: {:?}",
        events
            .iter()
            .map(|event| event.session_id.clone())
            .collect::<Vec<_>>()
    );

    claimed.state.record_active_turn_file_changes(&events);

    assert_eq!(
        claimed.record(|record| {
            record
                .active_turn_file_changes
                .iter()
                .map(|(path, kind)| (path.clone(), *kind))
                .collect::<Vec<_>>()
        }),
        [(
            edited.to_string_lossy().into_owned(),
            WorkspaceFileChangeKind::Modified
        )],
        "counted once, whichever scope carried it"
    );
    // The watcher cannot tell who made the edit, so the session whose scope
    // carried it counts it as well: the edit is attributed to both.
    let other_paths = {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&other)
            .expect("the other session exists");
        inner.sessions[index]
            .active_turn_file_changes
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(other_paths, [edited.to_string_lossy().into_owned()]);
    // The set is the turn's "files changed" summary, so the card lists it.
    let card = claimed.record(|record| {
        assert!(push_active_turn_file_changes_on_record(
            record,
            "message-root-card".to_owned()
        ));
        match record.session.messages.last() {
            Some(Message::FileChanges { files, .. }) => files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
            _ => panic!("the turn's file-change card should be the last message"),
        }
    });
    assert_eq!(card, [edited.to_string_lossy().into_owned()]);
}

#[test]
fn a_turn_admitted_with_a_named_root_marks_a_check_open_in_that_root() {
    // The turn-start sweep runs before the root is known; the admission that
    // installs it marks another session's check still open in that root.
    let label = "naming-admission-overlap";
    let grant_id = "turn-observation-naming-admission-overlap-grant";
    let claimed = ClaimedRoot::new(
        label,
        vec![
            bind_reply("turn-observation-naming-admission-overlap-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
        .expect("the root is named before the turn");
    let other = other_session(&claimed);
    let pending = Arc::new(EngramBasisCapture::default());
    let root = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    {
        let mut inner = claimed.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("the other session");
        let mut check = super::super::turn_checks::finished_check(0, pending.clone());
        check.target.root = root.clone();
        check.target.directory = root;
        inner.sessions[index].engram.active_turn_checks = vec![check];
    }
    let overlapped = || {
        let inner = claimed.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("the other session");
        inner.sessions[index].engram.active_turn_checks[0].overlapped
    };

    let dispatch = claimed.dispatch();
    assert!(
        !overlapped(),
        "the turn-start sweep sees only the workdir, a worktree of its own"
    );
    deliver_turn_dispatch(&claimed.state, dispatch)
        .expect("the begun root turn should reach the runtime");
    let _ = received_prompt(&claimed);

    assert!(overlapped(), "the admission that installs the root marks the check");
    pending.finish(None);
}

#[test]
fn a_delegated_session_is_not_told_to_name_a_root_it_cannot_name() {
    let child_bound = test_control_work_binding("naming-child-line", 1);
    assert_eq!(
        engram_bind_source_root_line(true, &[], None, None, &child_bound, "C:/child", false),
        None,
        "a delegated session gets no bind line"
    );
    assert!(
        engram_bind_source_root_line(false, &[], None, None, &child_bound, "C:/root", false)
            .is_some_and(|line| line.contains("termal_name_source_root")),
        "a root session is told how to name one"
    );
    let child_line = engram_source_root_withheld_line(
        FsPath::new("C:/elsewhere"),
        "C:/child",
        false,
        true,
        None,
    );
    assert!(!child_line.contains("termal_name_source_root"), "{child_line}");
    assert!(child_line.contains("run the tests in your workdir"), "{child_line}");
}

#[test]
fn a_name_landing_after_the_held_read_is_not_overwritten() {
    // Another holder names the work between this call's held-claims read and
    // its commit: the call's baseline is the list before the read, so it
    // sees the change and refuses rather than overwrite the newer name.
    let label = "naming-late-other";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    let store = claimed_root_store(&claimed);
    let held = claimed_root_held(label);
    let newer = EngramWorkSourceRoot {
        store,
        work_id: held.items[0].work_id.clone(),
        short_ref: format!("w-{label}"),
        claim_id: "claim-newer".to_owned(),
        claim_fence: 2,
        root: claimed.root.to_string_lossy().into_owned(),
        common_dir_key: "unused".to_owned(),
        named_by_session: "session-newer".to_owned(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation: 7,
    };
    let (state, landed) = (claimed.state.clone(), newer.clone());
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            state
                .inner
                .lock()
                .expect("state mutex poisoned")
                .engram_work_source_roots
                .push(landed);
        }));
    });

    let error = name_root(&claimed, label, Some(&worktree), vec![held])
        .expect_err("the newer name is not overwritten");

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("changed while it was being named"), "{}", error.message);
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_work_source_roots,
        vec![newer],
        "the newer name stands"
    );
}

#[test]
fn an_explicit_null_path_is_refused_on_the_route() {
    let label = "naming-route-null";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
        .expect("a root is named first");
    let app = app_router(claimed.state.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a test runtime");

    let response = runtime.block_on(request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{}/engram-source-root", claimed.session_id))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "work": format!("w-{label}"), "path": null }))
                    .expect("the body serializes"),
            ))
            .expect("the request builds"),
    ));

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_work_source_roots
            .len(),
        1,
        "a null path clears nothing"
    );
}

#[test]
fn a_name_given_again_after_a_clear_is_the_last_line_the_agent_reads() {
    // Name, clear, name the same root again before the next prompt: the
    // pending lines end on the name, the state the turn is measured in.
    let label = "naming-name-clear-name";
    let claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)]).expect("named");
    name_root(&claimed, label, None, vec![claimed_root_held(label)]).expect("cleared");
    name_root(&claimed, label, Some(&worktree), vec![claimed_root_held(label)])
        .expect("named again");

    let pending = pending_line(&claimed).expect("lines wait for the next prompt");
    let last = pending.lines().last().expect("a last line");
    assert!(last.contains("its named source root"), "{pending}");
    assert_eq!(
        pending.lines().filter(|line| line.contains("its named source root")).count(),
        1,
        "the name is not repeated: {pending}"
    );
}


#[test]
fn recognised_tests_name_the_bound_claim_and_the_other_live_named_claim() {
    for case in ["distinct", "shared", "expired"] {
        let label = format!("two-claims-{case}");
        let claimed = ClaimedRoot::new(
            &label,
            vec![
                bind_reply("two-claims-token"),
                grant_reply("two-claims-grant"),
                begin_reply("two-claims-grant"),
                checkpoint_reply("two-claims-grant"),
            ],
        );
        let first = add_claimed_root_worktree(&claimed.root);
        let second = if case == "shared" {
            first.clone()
        } else {
            let second = claimed.root.join(".worktrees/second");
            run_git_test_command(
                &claimed.root,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "second",
                    second.to_str().expect("UTF-8 fixture path"),
                ],
            );
            second
        };
        let mut held = claimed_root_held(&label);
        let first_ref = held.items[0].short_ref.clone();
        held.items.push(EngramHeldClaim {
            work_id: "work-second".to_owned(),
            short_ref: "w-second".to_owned(),
            claim_id: "claim-second".to_owned(),
            ..Default::default()
        });
        name_root(&claimed, &label, Some(&first), vec![held.clone()]).expect("first root named");
        claimed
            .state
            .name_engram_source_root(
                &claimed.session_id,
                EngramSourceRootRequest {
                    work: "w-second".to_owned(),
                    path: Some(Some(second.to_string_lossy().into_owned())),
                },
            )
            .expect("second root named");
        if case == "expired" {
            held.items.pop();
            claimed.transport.replace_held_claims([Ok(held)]);
        }
        deliver_turn_dispatch(&claimed.state, claimed.dispatch()).expect("turn delivered");
        let _ = received_prompt(&claimed);
        claimed.record(|record| record.engram.pending_source_root_line = None);
        claimed.state.note_engram_command_started(
            &claimed.session_id,
            &EngramObservationProvenance::Ambient,
            "test",
            Some("cargo test"),
            Some(second.to_str().expect("UTF-8 fixture path")),
        );
        let line = pending_line(&claimed).expect("test routing is explained");
        let checks = claimed.record(|record| record.engram.active_turn_checks.len());
        if case == "expired" {
            assert!(
                !line.contains("w-second"),
                "a released claim is not presented as held: {line}"
            );
            assert!(line.contains("run the tests in that root"), "{line}");
            assert_eq!(checks, 0);
        } else {
            assert!(
                line.contains(&first_ref),
                "the line identifies the bound claim: {line}"
            );
            assert!(
                line.contains("w-second"),
                "the line identifies the other held claim: {line}"
            );
            assert!(
                line.contains("focus") && line.contains("next turn"),
                "the remedy names the next turn: {line}"
            );
            assert_eq!(
                checks,
                usize::from(case == "shared"),
                "one bound claim only"
            );
        }
        claimed
            .state
            .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
            .expect("turn closes without rerouting its grant");
    }
}

#[test]
fn focusing_another_named_claim_rebinds_its_next_turn_and_test_evidence() {
    let label = "focused-named-claim";
    let claimed = ClaimedRoot::new(
        label,
        vec![
            bind_reply("first-token"),
            grant_reply("first-grant"),
            begin_reply("first-grant"),
            checkpoint_reply("first-grant"),
            status_reply("ready"),
            rebind_reply("second-token"),
            grant_reply("second-grant"),
            begin_reply("second-grant"),
            checkpoint_reply("second-grant"),
        ],
    );
    let first = add_claimed_root_worktree(&claimed.root);
    let second = claimed.root.join(".worktrees/second");
    run_git_test_command(
        &claimed.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "second",
            second.to_str().expect("UTF-8 fixture path"),
        ],
    );
    let mut held = claimed_root_held(label);
    let first_binding = held.items[0]
        .control_binding
        .clone()
        .expect("first binding");
    let second_binding = test_control_work_binding("focused-second", 1);
    held.items.push(EngramHeldClaim {
        work_id: second_binding.work_id.clone(),
        short_ref: "w-second".to_owned(),
        claim_id: second_binding.claim_id.clone(),
        claim_fence: second_binding.claim_fence,
        focused: false,
        control_binding: Some(second_binding.clone()),
    });
    name_root(&claimed, label, Some(&first), vec![held.clone()]).expect("first root named");
    claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: "w-second".to_owned(),
                path: Some(Some(second.to_string_lossy().into_owned())),
            },
        )
        .expect("second root named");
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).expect("first turn delivered");
    let _ = received_prompt(&claimed);
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .expect("first turn completes");

    held.items[0].focused = false;
    held.items[1].focused = true;
    let selected = select_engram_held_binding(
        held.items.clone(),
        held.omitted,
        EngramBindingPreference {
            current: Some(&first_binding),
            refused: &[],
        },
    );
    assert_eq!(
        selected,
        Some(second_binding.clone()),
        "focus wins over current binding"
    );
    claimed
        .transport
        .work_bindings
        .lock()
        .expect("binding reads mutex poisoned")
        .push_back(Ok(selected));
    claimed.transport.replace_held_claims([Ok(held)]);
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).expect("focused turn delivered");
    let _ = received_prompt(&claimed);
    claimed.record(|record| {
        assert_eq!(record.engram.work_binding, Some(second_binding.clone()));
        assert_eq!(
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .map(|root| root.short_ref.as_str()),
            Some("w-second")
        );
    });
    let wait = || {
        let captures = claimed.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .flat_map(|check| {
                    std::iter::once(check.start_basis.clone())
                        .chain(check.end.as_ref().map(|end| end.end_basis.clone()))
                })
                .collect::<Vec<_>>()
        });
        for capture in captures {
            capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
        }
    };
    claimed.state.note_engram_command_started(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        "focused-test",
        Some("cargo test"),
        Some(second.to_str().expect("UTF-8 fixture path")),
    );
    wait();
    assert_eq!(
        claimed.record(|record| record.engram.active_turn_checks.len()),
        1
    );
    claimed.state.note_engram_command_finished(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        "focused-test",
        "cargo test",
        "test result: ok. 1 passed",
        Some(EngramCommandExit::Code(0)),
    );
    wait();
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .expect("focused turn completes");
    let requests = claimed.transport.requests();
    let rebind = requests
        .iter()
        .filter(|r| r.request["operation"] == "session_bind")
        .last()
        .unwrap();
    assert_eq!(rebind.request["work_binding"], json!(second_binding));
    let checkpoint = requests
        .iter()
        .filter(|r| r.request["operation"] == "turn_checkpoint")
        .last()
        .unwrap();
    assert_eq!(
        checkpoint.request["verification_evidence"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        checkpoint.request["observations"][0]["outcome"],
        "succeeded"
    );
    assert_eq!(
        checkpoint.request["observations"][0]["source_basis"]["workspace_id"].as_str(),
        fs::canonicalize(&second).unwrap().to_str()
    );
}
