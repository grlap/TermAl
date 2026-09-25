//! Tests the work binding a session binds with, read from
//! `engram work core held` (Engram w-108a13d58018 criterion 3, tm-winf
//! step 3): the one-shot reader against a real process fixture and the
//! choice of one binding among the claims it lists.
//!
//! Owns the reader and selection tests of `engram_held_claims.rs`. Does not
//! own when the binding is read or refreshed (`engram_work_binding_refresh.rs`
//! and the adapter tests). Split out of `engram_host_adapter.rs`, where both
//! tests lived before, to sit beside the module they test.

use super::*;
#[test]
fn real_process_work_binding_reader_reads_the_held_claims() {
    let temp = TestTempRoot::create("termal-engram-work-binding");
    let project_file = temp.path().join(".engram-project");
    fs::write(&project_file, "held").expect("project fixture mode should write");
    #[cfg(windows)]
    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/tests/fixtures/engram-work-binding-fixture.ps1");
    #[cfg(not(windows))]
    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/tests/fixtures/engram-work-binding-fixture.sh");
    let connection = EngramConnectionConfig {
        binary_path,
        project_file: project_file.clone(),
        home: temp.path().to_path_buf(),
        project_root: temp.path().to_path_buf(),
        actor_id: "dev/codex".to_owned(),
        actor_context: Some("agent=codex;model=test;reasoning=high".to_owned()),
        session_id: "fixture-session".to_owned(),
    };
    // This tests the CLI protocol, not shell startup latency. Each invocation
    // records its actual phase; only the separate transport test expires a call.
    let phases = temp.path().join("work-read-phases");
    let read_binding = || {
        fs::write(&phases, "").expect("phase journal should reset");
        let started = std::time::Instant::now();
        let result = read_engram_work_binding_from_cli(
            &connection,
            EngramBindingPreference::default(),
            DEADLOCK_GUARD,
            false,
        );
        assert!(
            !result
                .as_ref()
                .is_err_and(|error| error.kind == EngramTransportErrorKind::Deadline),
            "work reader deadlock after {:?}; phases={:?}; result={result:?}",
            started.elapsed(),
            fs::read_to_string(&phases)
        );
        result
    };
    let read_phases = || {
        fs::read_to_string(&phases)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };

    // Two held claims, the older one focused: the focused claim is bound,
    // and fields the reader does not use are ignored.
    let binding = read_binding()
        .expect("work binding should be read")
        .expect("the focused held claim should carry a binding");
    assert_eq!(
        binding,
        EngramControlWorkBinding {
            root_execution_id: "root-fixture".to_owned(),
            work_id: "work-fixture".to_owned(),
            run_id: "run-fixture".to_owned(),
            work_revision: 17,
            claim_id: "claim-fixture".to_owned(),
            claim_fence: 23,
        }
    );
    assert_eq!(
        read_phases(),
        ["held"],
        "one read of the held claims, never a focus read"
    );

    // A held claim bind would refuse is printed with a null binding, or by an
    // older build without the key; either way it binds no work, as does
    // holding nothing. Only a present binding that does not decode, or output
    // without the items list, is an error.
    for mode in ["unbound-null", "unbound-absent", "no-claims"] {
        fs::write(&project_file, mode).expect("unbound fixture mode should write");
        assert_eq!(
            read_binding().unwrap_or_else(|error| panic!("{mode} read should succeed: {error:?}")),
            None,
            "{mode} must bind no work"
        );
        assert_eq!(read_phases(), ["held"], "{mode}");
    }
    for mode in ["malformed-binding", "malformed-output"] {
        fs::write(&project_file, mode).expect("malformed fixture mode should write");
        let error = read_binding().expect_err("output that does not decode must stay an error");
        assert_eq!(error.kind, EngramTransportErrorKind::Protocol, "{mode}");
        assert!(
            error
                .message
                .contains("invalid Engram work core held output"),
            "{mode}: {}",
            error.message
        );
    }

    fs::write(&project_file, "read-error-once").expect("read-error-once fixture mode should write");
    assert!(
        read_binding()
            .expect("database-lock read should retry once")
            .is_some(),
        "the one retry should recover the exact work binding"
    );
    assert_eq!(read_phases(), ["held", "held"]);

    fs::write(&project_file, "read-error").expect("read-error fixture mode should write");
    let error =
        read_binding().expect_err("a failed read must remain unknown instead of becoming no claim");
    assert_eq!(error.kind, EngramTransportErrorKind::Transport);
    assert_eq!(read_phases(), ["held", "held"]);
    assert!(
        error.message.contains("database is locked"),
        "the reader failure should preserve its diagnostic: {}",
        error.message
    );

    // An Engram build that predates the command fails every bind, so the
    // failure says what would fix it.
    fs::write(&project_file, "held-missing").expect("held-missing fixture mode should write");
    let error = read_binding().expect_err("a build without the command binds nothing");
    assert_eq!(error.kind, EngramTransportErrorKind::Transport);
    assert!(
        error.message.contains("unrecognized subcommand 'held'")
            && error.message.contains("install a newer Engram"),
        "{}",
        error.message
    );
}

/// A read's preference for a session bound with `binding`, with no refusal.
fn bound(binding: &EngramControlWorkBinding) -> EngramBindingPreference<'_> {
    EngramBindingPreference {
        current: Some(binding),
        refused: &[],
    }
}

#[test]
fn a_binding_engram_refused_gives_way_to_another_held_claim() {
    // Focus moved to a claim Engram refused as stale (a pending handoff, an
    // ancestor it no longer accepts): the session keeps the claim it is bound
    // to rather than binding no work, since a note on another item must not
    // unbind it.
    let current = test_control_work_binding("refused-current", 1);
    let refused = test_control_work_binding("refused-focused", 1);
    let newest = test_control_work_binding("refused-newest", 1);
    let claim = |binding: &EngramControlWorkBinding, focused: bool| EngramHeldClaim {
        work_id: binding.work_id.clone(),
        focused,
        control_binding: Some(binding.clone()),
    };

    assert_eq!(
        select_engram_held_binding(
            vec![
                claim(&newest, false),
                claim(&current, false),
                claim(&refused, true)
            ],
            0,
            EngramBindingPreference {
                current: Some(&current),
                refused: std::slice::from_ref(&refused),
            },
        ),
        Some(current.clone()),
        "the bound claim, not the refused focused one"
    );
    let revised = EngramControlWorkBinding {
        work_revision: 2,
        claim_fence: refused.claim_fence + 1,
        ..refused.clone()
    };
    assert_eq!(
        select_engram_held_binding(
            vec![claim(&current, false), claim(&revised, true)],
            0,
            EngramBindingPreference {
                current: Some(&current),
                refused: std::slice::from_ref(&refused),
            },
        ),
        Some(revised),
        "a refused claim that moved on is another binding"
    );
    for omitted in [0, 3] {
        assert_eq!(
            select_engram_held_binding(
                vec![claim(&newest, false), claim(&current, false)],
                omitted,
                EngramBindingPreference {
                    current: Some(&current),
                    refused: std::slice::from_ref(&current),
                },
            ),
            Some(newest.clone()),
            "a refused bound claim gives way to the newest other (omitted={omitted})"
        );
    }
    assert_eq!(
        select_engram_held_binding(
            vec![claim(&newest, false)],
            3,
            EngramBindingPreference {
                current: Some(&current),
                refused: std::slice::from_ref(&current),
            },
        ),
        Some(newest.clone()),
        "a refused bound claim is not kept for being left out of a capped list"
    );
    assert_eq!(
        select_engram_held_binding(
            vec![claim(&refused, true)],
            0,
            EngramBindingPreference {
                current: None,
                refused: std::slice::from_ref(&refused),
            },
        ),
        None,
        "with nothing else held, no work"
    );
    // Two claims Engram refused are both left out: neither takes the other's
    // turn, and a valid claim, or no work, binds instead.
    let both = [current.clone(), refused.clone()];
    assert_eq!(
        select_engram_held_binding(
            vec![
                claim(&newest, false),
                claim(&current, false),
                claim(&refused, true)
            ],
            0,
            EngramBindingPreference {
                current: Some(&current),
                refused: &both,
            },
        ),
        Some(newest.clone()),
        "both refused claims give way to the valid one"
    );
    assert_eq!(
        select_engram_held_binding(
            vec![claim(&current, false), claim(&refused, true)],
            3,
            EngramBindingPreference {
                current: Some(&current),
                refused: &both,
            },
        ),
        None,
        "with only refused claims held, no work"
    );
}

#[test]
fn a_held_binding_is_chosen_by_focus_then_the_current_claim_then_the_newest() {
    let newest = test_control_work_binding("held-newest", 1);
    let current = test_control_work_binding("held-current", 1);
    let focused = test_control_work_binding("held-focused", 1);
    let claim = |binding: &EngramControlWorkBinding, focused: bool| EngramHeldClaim {
        work_id: binding.work_id.clone(),
        focused,
        control_binding: Some(binding.clone()),
    };
    let unbindable = |work_id: &str, focused: bool| EngramHeldClaim {
        work_id: work_id.to_owned(),
        focused,
        control_binding: None,
    };

    assert_eq!(
        select_engram_held_binding(
            vec![
                claim(&newest, false),
                claim(&current, false),
                claim(&focused, true)
            ],
            0,
            bound(&current),
        ),
        Some(focused.clone()),
        "the focused claim first"
    );
    let revised_current = EngramControlWorkBinding {
        work_revision: 2,
        claim_fence: current.claim_fence + 1,
        ..current.clone()
    };
    assert_eq!(
        select_engram_held_binding(
            vec![
                claim(&newest, false),
                claim(&revised_current, false),
                unbindable("work-held-offered", true)
            ],
            0,
            bound(&current),
        ),
        Some(revised_current.clone()),
        "then the bound claim, with its current revision and fence, when focus is \
         on an item bind would refuse or on no held item"
    );
    assert_eq!(
        select_engram_held_binding(
            vec![claim(&newest, false), claim(&current, false)],
            0,
            bound(&test_control_work_binding("held-released", 1)),
        ),
        Some(newest.clone()),
        "then the most recently claimed"
    );
    assert_eq!(
        select_engram_held_binding(
            vec![unbindable("work-a", true), unbindable("work-b", false)],
            0,
            bound(&current),
        ),
        None,
        "claims bind would refuse are never chosen"
    );
    assert_eq!(
        select_engram_held_binding(Vec::new(), 0, EngramBindingPreference::default()),
        None
    );

    // Engram lists at most sixteen claims, newest first: a bound claim left
    // out of a capped list may still be held, so it is kept, not switched.
    assert_eq!(
        select_engram_held_binding(vec![claim(&newest, false)], 3, bound(&current)),
        Some(current.clone()),
        "an unlisted current claim under a capped list is kept"
    );
    assert_eq!(
        select_engram_held_binding(
            vec![claim(&newest, false), unbindable(&current.work_id, false)],
            3,
            bound(&current),
        ),
        Some(newest.clone()),
        "a listed current claim bind would refuse is not kept"
    );
}
