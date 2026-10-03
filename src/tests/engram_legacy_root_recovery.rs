//! Restore local-only naming bytes, then trace a prospective canonical name.
use super::*;

#[test]
fn source_root_legacy_recovery_between_admission_and_handoff_preserves_opening_and_user_quote() {
    let label = "legacy-handoff-recovery";
    let grant = "legacy-handoff-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("legacy-handoff-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    restore_local_only_root(&claimed, label, &worktree);
    let mut dispatch = claimed.dispatch();
    assert!(matches!(
        claimed.state.prepare_engram_turn_delivery_off_lock(
            &claimed.session_id,
            dispatch.engram_dispatch_generation().unwrap()
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let line = claimed.record(|record| {
        record
            .engram
            .source_root_notices
            .first()
            .unwrap()
            .line()
            .to_owned()
    });
    let TurnDispatch::PersistentCodex { command, .. } = &mut dispatch else {
        panic!("Codex fixture");
    };
    command
        .prompt
        .push_str(&format!("\nUser quotation: {line}"));
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert!(matches!(
        handoff_prepared_turn_dispatch(&claimed.state, dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered
    ));
    let prompt = received_prompt(&claimed);
    assert!(
        prompt.contains(&format!("User quotation: {line}")),
        "{prompt}"
    );
    assert_eq!(
        prompt
            .matches("Source-root authority recovery remains incomplete")
            .count(),
        1,
        "only the user quotation remains: {prompt}"
    );
    assert!(
        prompt.contains("This turn's source-root binding is unconfirmed"),
        "{prompt}"
    );
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    observe_legacy_check(&claimed, "legacy-handoff-check", &worktree);
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .unwrap();
    let checkpoint = legacy_checkpoint(&claimed, grant);
    assert!(
        checkpoint
            .get("verification_evidence")
            .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
        "{checkpoint}"
    );
}

#[test]
fn source_root_legacy_unresolved_warning_delivers_and_withholds_check() {
    let label = "legacy-unresolved-notice";
    let grant = "legacy-unresolved-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("legacy-unresolved-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    restore_local_only_root(&claimed, label, &worktree);
    let dispatch = claimed.dispatch();
    assert!(matches!(
        claimed.state.prepare_engram_turn_delivery_off_lock(
            &claimed.session_id,
            dispatch.engram_dispatch_generation().unwrap()
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert!(claimed.record(|record| !record.engram.source_root_notices.is_empty()));
    assert!(matches!(
        handoff_prepared_turn_dispatch(&claimed.state, dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered
    ));
    let prompt = received_prompt(&claimed);
    assert!(
        prompt.contains("Source-root authority recovery remains incomplete"),
        "{prompt}"
    );
    assert!(
        prompt.contains("This turn's source-root binding is unconfirmed"),
        "{prompt}"
    );
    assert!(
        claimed.record(|record| record.engram.source_root_notices.is_empty()),
        "accepted send acknowledges current status"
    );
    observe_legacy_check(&claimed, "legacy-unresolved-check", &worktree);
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .unwrap();
    let checkpoint = legacy_checkpoint(&claimed, grant);
    assert!(
        checkpoint
            .get("verification_evidence")
            .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
        "{checkpoint}"
    );
    assert!(
        checkpoint["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|observation| observation.get("source_basis").is_none_or(Value::is_null)),
        "{checkpoint}"
    );
}

#[test]
fn source_root_legacy_recovery_warning_survives_failed_provider_send() {
    let label = "legacy-failed-notice";
    let grant = "legacy-failed-grant";
    let mut claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("legacy-failed-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    restore_local_only_root(&claimed, label, &worktree);
    let dispatch = claimed.dispatch();
    assert!(matches!(
        claimed.state.prepare_engram_turn_delivery_off_lock(
            &claimed.session_id,
            dispatch.engram_dispatch_generation().unwrap()
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    claimed.close_runtime_channel();
    assert!(handoff_prepared_turn_dispatch(&state, dispatch).is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        !record.engram.source_root_notices.is_empty(),
        "failed send is not delivery"
    );
    assert!(!record.engram.opening_diagnostic.as_ref().unwrap().delivered);
}

fn restore_local_only_root(claimed: &ClaimedRoot, label: &str, worktree: &FsPath) {
    prepare_claimed_root_naming(claimed, label);
    let store = claimed_root_store(claimed);
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
            work_id: binding.work_id,
            short_ref: format!("w-{label}"),
            claim_id: binding.claim_id,
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
    *claimed.state.inner.lock().unwrap() = restored;
    claimed.record(|record| record.runtime = runtime);
    claimed
        .state
        .install_test_engram_budget_clock(EngramBudgetClock::scripted());
    install_control_only_transport(&claimed.state, claimed.transport.clone());
    prepare_claimed_root_naming(claimed, label);
}

fn observe_legacy_check(claimed: &ClaimedRoot, key: &str, worktree: &FsPath) {
    claimed.state.note_engram_command_started(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        key,
        Some("cargo test"),
        Some(worktree.to_str().unwrap()),
    );
    claimed.state.note_engram_command_finished(
        &claimed.session_id,
        &EngramObservationProvenance::Ambient,
        key,
        "cargo test",
        "test result: ok. 1 passed",
        Some(EngramCommandExit::Code(0)),
    );
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
}

fn legacy_checkpoint(claimed: &ClaimedRoot, grant: &str) -> Value {
    claimed
        .transport
        .requests()
        .into_iter()
        .find(|request| {
            request.request["operation"] == "turn_checkpoint"
                && request.request["grant_id"] == grant
        })
        .unwrap()
        .request
}

#[test]
fn source_root_legacy_forward_name_correlates_fresh_capture_warning_and_check() {
    let label = "legacy-forward-correlation";
    let old_grant = "legacy-uncertain-grant";
    let fresh_grant = "legacy-confirmed-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("legacy-forward-token"),
            grant_reply(old_grant),
            begin_reply(old_grant),
            checkpoint_reply(old_grant),
            grant_reply(fresh_grant),
            begin_reply(fresh_grant),
            checkpoint_reply(fresh_grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    restore_local_only_root(&claimed, label, &worktree);
    let store = claimed_root_store(&claimed);
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let old_prompt = received_prompt(&claimed);
    assert!(
        old_prompt.contains("canonical naming history"),
        "{old_prompt}"
    );
    claimed.record(|record| {
        assert_eq!(record.engram.active_grant_id.as_deref(), Some(old_grant));
        assert_eq!(
            record.engram.active_turn_root_capture,
            Some(EngramRootCapture::Unconfirmed)
        );
        assert!(record.engram.active_turn_start_basis.is_none());
    });
    // The actual recovery publisher creates the queued current-status line.
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let issued_warning = claimed.record(|record| {
        record
            .engram
            .source_root_notices
            .first()
            .unwrap()
            .line()
            .to_owned()
    });
    assert!(
        issued_warning.contains("recovery remains incomplete"),
        "{issued_warning}"
    );
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert!(
        named.generation > 7,
        "a forward name must never reuse the legacy generation"
    );
    {
        let inner = claimed.state.inner.lock().unwrap();
        let history = inner
            .engram_work_naming_history
            .iter()
            .find(|history| history.work_id == binding.work_id)
            .unwrap();
        assert_eq!(history.epoch, ENGRAM_NAMING_HISTORY_EPOCH);
        assert!(history.frontier.is_some());
        assert!(!engram_authority_work_unresolved(
            &inner,
            &store,
            &binding.work_id
        ));
    }
    // Recovery cannot relabel the original admitted opening or its check.
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    observe_legacy_check(&claimed, "legacy-old-check", &worktree);
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .unwrap();
    let old = legacy_checkpoint(&claimed, old_grant);
    assert!(
        old.get("verification_evidence")
            .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
        "{old}"
    );
    assert!(
        old["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|observation| observation.get("source_basis").is_none_or(Value::is_null)),
        "{old}"
    );
    let historical_refusal = "[TermAl] A recognised test earlier earned no credit; its source-root binding was unconfirmed.";
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line(historical_refusal.to_owned())
    });
    claimed
        .transport
        .work_bindings
        .lock()
        .unwrap()
        .push_back(Ok(Some(binding)));
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let fresh_prompt = received_prompt(&claimed);
    claimed.record(|record| {
        assert_eq!(record.engram.active_grant_id.as_deref(), Some(fresh_grant));
        assert!(matches!(record.engram.active_turn_root_capture, Some(EngramRootCapture::Recorded { generation, state: EngramSourceRootState::Named, .. }) if generation == named.generation as i64));
        let basis = record.engram.active_turn_start_basis.as_ref().unwrap();
        assert_eq!(basis.source_root_generation, Some(named.generation as i64));
        assert_eq!(basis.workspace_id, named.root.clone().unwrap());
        assert!(record.engram.opening_diagnostic.is_none());
    });
    assert!(fresh_prompt.contains(historical_refusal), "{fresh_prompt}");
    observe_legacy_check(&claimed, "legacy-fresh-check", &worktree);
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .unwrap();
    let fresh = legacy_checkpoint(&claimed, fresh_grant);
    assert_eq!(
        fresh["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "{fresh}"
    );
    eprintln!(
        "legacy causal trace: old_grant={old_grant} Unconfirmed; warning={issued_warning:?}; forward_generation={}; fresh_grant={fresh_grant} Recorded; fresh_prompt={fresh_prompt:?}; verification_count=1",
        named.generation
    );
    assert!(
        !fresh_prompt.contains("Source-root authority recovery remains incomplete"),
        "a confirmed fresh opening must not carry obsolete current-status warning: {fresh_prompt}"
    );
}
