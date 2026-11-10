//! Passive local-only naming history and prospective ordinary trace ownership.
use super::*;

const RECOVERY_WARNING: &str =
    "[TermAl] Retained source-root history requires repair before a fresh evaluation:";

fn legacy_store(turn: &EOCheckedTurn) -> EngramAuthorityStoreKey {
    let inner = turn.state.inner.lock().unwrap();
    engram_project_for_session_locked(&inner, &turn.session_id)
        .unwrap()
        .engram
        .as_ref()
        .unwrap()
        .authority_store_key
        .clone()
        .unwrap()
}

fn legacy_worktree(turn: &EOCheckedTurn) -> PathBuf {
    fs::write(turn.root.join(".gitignore"), ".worktrees/\n").unwrap();
    run_git_test_command(&turn.root, &["add", ".gitignore", ".engram-project"]);
    run_git_test_command(
        &turn.root,
        &["commit", "--quiet", "-m", "declare historical worktree"],
    );
    let path = turn.root.join(".worktrees").join("historical-naming");
    run_git_test_command(
        &turn.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "historical-naming",
            path.to_str().unwrap(),
        ],
    );
    path
}

fn name_recovered_root(turn: &EOCheckedTurn, root: &FsPath) -> EngramSourceRootResponse {
    let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    turn.state
        .name_engram_source_root(
            &turn.session_id,
            EngramSourceRootRequest {
                work,
                path: Some(Some(root.to_string_lossy().into_owned())),
            },
        )
        .unwrap()
}

fn recovery_notice(turn: &EOCheckedTurn) -> String {
    turn.state
        .recover_engram_authority_runs(&turn.session_id, Duration::from_secs(2));
    turn.record(|record| {
        record
            .engram
            .pending_source_root_line
            .as_deref()
            .unwrap()
            .lines()
            .find(|line| line.starts_with(RECOVERY_WARNING))
            .expect("the current public warning carries unresolved canonical history")
            .to_owned()
    })
}

fn observe_legacy_check(turn: &EOCheckedTurn, worktree: &FsPath) {
    let cwd = worktree.to_string_lossy();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(
            "historical-check",
            "cargo test",
            Some("cargo test"),
            Some(&cwd),
        )
        .unwrap();
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "historical-check",
            "cargo test",
            "test result: ok. 1 passed",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    turn.wait_for_snapshots();
}

fn finish_with_unresolved_local_root(turn: &EOCheckedTurn) {
    let trace = turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert_eq!(
        trace.opening_root.as_ref().unwrap().provenance,
        EngramRootCapture::Unconfirmed
    );
    let scope =
        EngramRuntimeClosingScope::capture(&turn.state.inner.lock().unwrap(), &turn.session_id)
            .unwrap();
    assert!(matches!(
        &scope.root,
        EngramNamedRootState::Bound { generation: 7, .. }
    ));
    let expected_error = format!(
        "not reported: scope changed; pinned [{}], current [canonical root None]",
        scope.label()
    );
    let (installed, installed_rx) = mpsc::channel();
    let inner = turn.state.inner.clone();
    let session = turn.session_id.clone();
    let expected_turn = trace.host_turn_ref.clone();
    // The existing worker publishes this terminal local result before it
    // reports idle. No external observation can certify the unexplained root.
    *turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = Some(Arc::new(move |phase| {
        if phase != "closing_installed" {
            return;
        }
        let failed = {
            let inner = inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|job| {
                    job.trace.host_turn_ref == expected_turn && job.terminal_failure.is_some()
                })
                .cloned()
        };
        if let Some(failed) = failed {
            let _ = installed.send(failed);
        }
    }));
    turn.end();
    let failed = phase_sync::receive(
        &installed_rx,
        "the unresolved local root retains its withheld closing",
    );
    *turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = None;
    assert_eq!(failed.scope, scope);
    assert_eq!(failed.trace.host_turn_ref, trace.host_turn_ref);
    assert_eq!(failed.trace.opening_root, trace.opening_root);
    assert_eq!(failed.trace.commands, trace.commands);
    assert_eq!(
        failed.trace.observed_outcome,
        Some(EngramObservedTurnOutcome::Succeeded)
    );
    assert_eq!(failed.attempts, 1);
    assert_eq!(failed.last_error.as_deref(), Some(expected_error.as_str()));
    assert_eq!(
        failed.terminal_failure.as_deref(),
        Some(expected_error.as_str())
    );
    turn.record(|record| {
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert_eq!(
            record.engram.unadmitted_closing_jobs.as_slice(),
            &[failed.clone()]
        );
        assert!(record.engram.unadmitted_reports.is_empty());
    });
    turn.state.shutdown_persist_blocking();
    let restored = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &restored.sessions[restored.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(record.engram.unadmitted_closing_jobs.as_slice(), &[failed]);
    assert!(record.engram.unadmitted_reports.is_empty());
    assert!(
        !turn
            .transport
            .requests()
            .iter()
            .any(|sent| sent.request["operation"] == "execution_observe"),
        "unexplained local history produces no external observation or source credit"
    );
    assert_ordinary_turn_has_no_native_requests(turn);
}

#[test]
fn recovered_naming_preserves_a_user_quotation_without_repeating_obsolete_status() {
    let turn = EOCheckedTurn::idle("historical-quoted-warning", true);
    let worktree = legacy_worktree(&turn);
    restore_local_only_root(&turn, "historical-quoted-warning", &worktree);
    let line = recovery_notice(&turn);
    turn.begin("Observe the historical local-only root.");
    assert_eq!(
        turn.record(|record| record.engram.observed_opening().unwrap().provenance.clone()),
        EngramRootCapture::Unconfirmed
    );
    finish_with_unresolved_local_root(&turn);
    name_recovered_root(&turn, &worktree);
    let prompt = turn.begin_prompt(&format!("User quotation: {line}")).prompt;
    assert!(
        prompt.contains(&format!("User quotation: {line}")),
        "{prompt}"
    );
    assert_eq!(
        prompt.matches(RECOVERY_WARNING).count(),
        1,
        "only the user quotation remains after prospective recovery: {prompt}"
    );
    assert!(matches!(
        turn.record(|record| record.engram.observed_opening().unwrap().provenance.clone()),
        EngramRootCapture::Recorded { .. }
    ));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn unresolved_local_history_delivers_ordinary_input_without_fabricating_native_evidence() {
    let turn = EOCheckedTurn::idle("historical-unresolved-warning", true);
    let worktree = legacy_worktree(&turn);
    restore_local_only_root(&turn, "historical-unresolved-warning", &worktree);
    let line = recovery_notice(&turn);
    let prompt = turn.begin_prompt("Inspect the retained history.").prompt;
    assert!(prompt.contains(&line), "{prompt}");
    assert!(
        prompt.contains("current absence cannot erase unexplained historical naming"),
        "{prompt}"
    );
    assert!(
        turn.record(|record| record
            .engram
            .pending_source_root_line
            .as_deref()
            .is_none_or(|pending| !pending.lines().any(|pending| pending == line.as_str()))),
        "only accepted provider delivery acknowledges the current notice"
    );
    assert_eq!(
        turn.record(|record| record.engram.observed_opening().unwrap().provenance.clone()),
        EngramRootCapture::Unconfirmed
    );
    observe_legacy_check(&turn, &worktree);
    finish_with_unresolved_local_root(&turn);
}

#[test]
fn historical_recovery_notice_survives_failed_ordinary_provider_send() {
    let mut turn = EOCheckedTurn::idle("historical-failed-provider", true);
    let worktree = legacy_worktree(&turn);
    restore_local_only_root(&turn, "historical-failed-provider", &worktree);
    let line = recovery_notice(&turn);
    let dispatch = match turn
        .state
        .dispatch_turn(
            &turn.session_id,
            SendMessageRequest {
                text: "Inspect history.".to_owned(),
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
        DispatchTurnResult::Queued => panic!("the real idle owner must dispatch"),
    };
    // Drop the actual runtime's receiver, without a parallel runtime fixture.
    let (_unused_sender, disconnected) = mpsc::channel();
    turn._runtime_rx = Arc::new(Mutex::new(disconnected));
    assert!(deliver_turn_dispatch(&turn.state, dispatch).is_err());
    assert!(turn.record(|record| {
        record
            .engram
            .pending_source_root_line
            .as_deref()
            .is_some_and(|pending| pending.lines().any(|pending| pending == line.as_str()))
    }));
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn a_forward_name_is_prospective_and_never_upgrades_an_earlier_uncertain_trace() {
    let turn = EOCheckedTurn::idle("historical-forward-correlation", true);
    let worktree = legacy_worktree(&turn);
    restore_local_only_root(&turn, "historical-forward-correlation", &worktree);
    let store = legacy_store(&turn);
    let line = recovery_notice(&turn);
    let old_prompt = turn.begin_prompt("Inspect the old local history.").prompt;
    assert!(old_prompt.contains(&line), "{old_prompt}");
    let original = turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert_eq!(
        original.opening_root.as_ref().unwrap().provenance,
        EngramRootCapture::Unconfirmed
    );
    let named = name_recovered_root(&turn, &worktree);
    assert!(named.generation > 7);
    turn.record(|record| {
        let opening = record.engram.observed_opening().unwrap();
        assert_eq!(opening.provenance, EngramRootCapture::Unconfirmed);
        assert_eq!(
            record.engram.observed_turn_ref(),
            Some(original.host_turn_ref.as_str())
        );
    });
    {
        let inner = turn.state.inner.lock().unwrap();
        let work = inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
            .engram
            .work_binding
            .as_ref()
            .unwrap()
            .work_id
            .clone();
        let history = inner
            .engram_work_naming_history
            .iter()
            .find(|h| h.work_id == work)
            .unwrap();
        assert_eq!(history.epoch, ENGRAM_NAMING_HISTORY_EPOCH);
        assert!(history.frontier.is_some());
        assert!(!engram_authority_work_unresolved(&inner, &store, &work));
    }
    observe_legacy_check(&turn, &worktree);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    let historical = "[TermAl] An earlier observation had unconfirmed source attribution.";
    turn.record_mut(|record| {
        record
            .engram
            .set_pending_source_root_line(historical.to_owned())
    });
    let fresh = turn
        .begin_prompt("Inspect the confirmed forward name.")
        .prompt;
    assert!(fresh.contains(historical), "{fresh}");
    assert!(!fresh.contains(RECOVERY_WARNING), "{fresh}");
    turn.record(|record| {
        let root = record.engram.observed_source_root().unwrap();
        assert_eq!(root.generation, named.generation);
        assert_eq!(Some(&root.root), named.root.as_ref());
        assert!(
            matches!(record.engram.observed_opening().unwrap().provenance.clone(),
            EngramRootCapture::Recorded { generation, state: EngramSourceRootState::Named, .. }
                if generation == named.generation as i64)
        );
    });
    observe_legacy_check(&turn, &worktree);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

fn restore_local_only_root(claimed: &EOCheckedTurn, label: &str, worktree: &FsPath) {
    let store = legacy_store(claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let (root, common_dir_key) = validate_engram_source_root(
        worktree.to_str().unwrap(),
        claimed.root.to_str().unwrap(),
        claimed.root.to_str().unwrap(),
    )
    .unwrap();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_source_root_generation = 7;
        inner.engram_work_source_roots.push(EngramWorkSourceRoot {
            store,
            work_id: binding.work_id.clone(),
            short_ref: format!("w-{label}"),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            root,
            common_dir_key,
            named_by_session: claimed.session_id.clone(),
            named_at: "2026-09-28T00:00:00Z".to_owned(),
            generation: 7,
        });
        assert!(inner.engram_work_naming_history.is_empty());
        assert!(inner.engram_named_root_journal.is_empty());
    }
    // Actual SQLite bytes omit canonical history, as the local-only host did.
    let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    persist_delta_via_cache(
        &mut SqlitePersistConnectionCache::new(),
        claimed.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap();
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(restored.engram_work_naming_history[0].epoch, 0);
    assert_eq!(restored.engram_work_naming_history[0].known_generation, 7);
    assert!(restored.engram_work_naming_history[0].frontier.is_none());
    assert!(restored.engram_named_root_journal.is_empty());
    let runtime = claimed.record(|record| record.runtime.clone());
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        // Preserve the fixture's explicit owner replies across
        // the actual reload; persisted state deliberately omits test hooks.
        let capture = inner
            .test_observation_source_owner
            .clone()
            .expect("the ordinary fixture owns its source-owner replies");
        *inner = restored;
        inner.test_observation_source_owner = Some(capture);
    }
    claimed.record_mut(|record| {
        record.runtime = runtime;
        // Explicit scripted fixture association after reload; not attestation.
        record.engram.work_binding = Some(binding);
    });
    // The restored state starts unread and with no clock chosen; it chooses
    // a scripted clock before anything snapshots it.
    claimed.state.select_test_scripted_engram_budget_clock();
    install_control_only_transport(&claimed.state, claimed.transport.clone());
}

/// A repair line retires only for the exact store and work whose history an
/// acknowledged forward name made current. A line given for the same work in
/// another store, or for an earlier work, stays pending, as does an unrelated
/// line, in order; a line given for two origins stays until both retire.
#[test]
fn a_forward_name_retires_only_the_repair_line_of_its_own_store_and_work() {
    let store = |name: &str| EngramAuthorityStoreKey {
        database_path: PathBuf::from(format!("{name}.sqlite")),
        project_id: name.to_owned(),
    };
    let (named, other) = (store("named-store"), store("other-store"));
    let line = |reason: &str| {
        format!(
            "{RECOVERY_WARNING} {reason}. Source, test and evaluation evidence remain withheld."
        )
    };
    let unrelated = "[TermAl] An unrelated pending fact.".to_owned();
    let pending = |engram: &EngramSessionState| -> Vec<String> {
        engram
            .pending_source_root_line
            .as_deref()
            .map_or_else(Vec::new, |pending| {
                pending.lines().map(str::to_owned).collect()
            })
    };

    let mut engram = EngramSessionState::default();
    engram.set_pending_retained_history_repair_line(line("named work"), &named, "work-w");
    engram.set_pending_retained_history_repair_line(line("other store"), &other, "work-w");
    engram.set_pending_retained_history_repair_line(line("earlier work"), &named, "work-earlier");
    engram.set_pending_source_root_line(unrelated.clone());
    engram.retire_retained_history_repair_lines(&named, "work-w");
    assert_eq!(
        pending(&engram),
        vec![line("other store"), line("earlier work"), unrelated.clone()],
        "only the named store and work lose their repair line"
    );

    let mut shared = EngramSessionState::default();
    shared.set_pending_retained_history_repair_line(line("shared"), &named, "work-w");
    shared.set_pending_retained_history_repair_line(line("shared"), &other, "work-w");
    shared.set_pending_source_root_line(unrelated.clone());
    shared.retire_retained_history_repair_lines(&named, "work-w");
    assert_eq!(
        pending(&shared),
        vec![line("shared"), unrelated.clone()],
        "the same line given for another store still tells the truth"
    );
    shared.retire_retained_history_repair_lines(&other, "work-w");
    assert_eq!(
        pending(&shared),
        vec![unrelated],
        "retired once both origins are current"
    );
}

/// A repair line the agent was given leaves with its origin. Given again in
/// the same words for another store, only that new origin holds it, so the
/// new store's forward name retires it.
#[test]
fn a_delivered_repair_line_given_again_retires_with_its_new_origin() {
    let store = |name: &str| EngramAuthorityStoreKey {
        database_path: PathBuf::from(format!("{name}.sqlite")),
        project_id: name.to_owned(),
    };
    let (first, second) = (store("first-store"), store("second-store"));
    let text = format!(
        "{RECOVERY_WARNING} the original canonical association is unavailable. \
        Source, test and evaluation evidence remain withheld."
    );

    // Through the delivery owner: the runtime accepted the prompt carrying it.
    let state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record
            .engram
            .set_pending_retained_history_repair_line(text.clone(), &first, "work-w");
        let generation = record.active_turn_generation;
        record.engram.source_root_line_delivery = Some((text.clone(), generation));
        generation
    };
    state.acknowledge_engram_source_root_line_delivery(&session, generation);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let engram = &mut inner.sessions[index].engram;
        assert!(engram.pending_source_root_line.is_none());
        assert!(
            engram.retained_history_repair_origins.is_empty(),
            "a delivered line keeps no origin"
        );
        engram.set_pending_retained_history_repair_line(text.clone(), &second, "work-w");
        engram.retire_retained_history_repair_lines(&second, "work-w");
        assert!(
            engram.pending_source_root_line.is_none(),
            "the second store's resolved warning retires"
        );
    }

    // The setter alone also drops an origin whose line already left.
    let mut engram = EngramSessionState::default();
    engram.set_pending_retained_history_repair_line(text.clone(), &first, "work-w");
    engram.pending_source_root_line = None;
    engram.set_pending_retained_history_repair_line(text.clone(), &second, "work-w");
    engram.retire_retained_history_repair_lines(&second, "work-w");
    assert!(
        engram.pending_source_root_line.is_none(),
        "no stale origin holds the reused line"
    );

    // Ordinary host lines that push the repair line out of the capped pending
    // lines take its origin with it.
    let mut evicted = EngramSessionState::default();
    evicted.set_pending_retained_history_repair_line(text.clone(), &first, "work-w");
    for ordinal in 0..4 {
        evicted.set_pending_source_root_line(format!("[TermAl] Ordinary host line {ordinal}."));
    }
    assert!(
        !evicted
            .pending_source_root_line
            .as_deref()
            .unwrap()
            .contains(RECOVERY_WARNING)
    );
    assert!(
        evicted.retained_history_repair_origins.is_empty(),
        "an evicted line keeps no origin"
    );
    evicted.set_pending_retained_history_repair_line(text.clone(), &second, "work-w");
    evicted.retire_retained_history_repair_lines(&second, "work-w");
    assert!(
        !evicted
            .pending_source_root_line
            .as_deref()
            .unwrap_or_default()
            .contains(RECOVERY_WARNING),
        "the second store's resolved warning retires after the first one was evicted"
    );
}
