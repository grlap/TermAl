use super::*;

#[test]
fn source_root_restored_selection_does_not_deliver_an_obsolete_rename_warning() {
    let label = "confirmation-restored-selection";
    let grant = "confirmation-restored-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("confirmation-restored-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    let original = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (token, binding, bound) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone().unwrap(),
            record.engram.named_root.clone(),
        )
    });
    let warning = "Engram no longer confirms this claim's local source-root selection";
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            bound,
            original.generation,
        )
        .unwrap();
    assert!(!pending_line(&claimed).unwrap_or_default().contains(warning));

    // A real canonical Ended event invalidates the selection and issues the warning.
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            original.generation,
        )
        .unwrap();
    assert!(claimed.record(|record| {
        record
            .engram
            .source_root_notices
            .iter()
            .any(|notice| notice.line().contains(warning))
    }));
    let store = claimed_root_store(&claimed);
    assert!(
        engram_work_source_root_for_claim(
            &claimed.state.inner.lock().unwrap().engram_work_source_roots,
            &store,
            &binding.work_id,
            &binding.claim_id,
        )
        .is_none()
    );

    let restored = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert!(restored.generation > original.generation);
    let kept = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert_eq!(kept.generation, restored.generation);
    {
        let inner = claimed.state.inner.lock().unwrap();
        let selection = engram_work_source_root_for_claim(
            &inner.engram_work_source_roots,
            &store,
            &binding.work_id,
            &binding.claim_id,
        )
        .unwrap();
        assert_eq!(selection.generation, restored.generation);
        assert_eq!(selection.root, restored.root.clone().unwrap());
        assert!(!engram_authority_work_unresolved(
            &inner,
            &store,
            &binding.work_id
        ));
    }
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let prompt = received_prompt(&claimed);
    claimed.record(|record| {
        assert!(matches!(record.engram.active_turn_root_capture,
            Some(EngramRootCapture::Recorded { state: EngramSourceRootState::Named,
                generation, .. }) if generation == restored.generation as i64));
        assert_eq!(
            record
                .engram
                .active_turn_start_basis
                .as_ref()
                .unwrap()
                .source_root_generation,
            Some(restored.generation as i64)
        );
        assert!(record.engram.opening_diagnostic.is_none());
    });
    eprintln!(
        "selection warning causal trace: original={}, canonical Ended removed selection; restored={}, kept={}, fresh Recorded prompt={prompt:?}",
        original.generation, restored.generation, kept.generation
    );
    assert!(
        !prompt.contains(warning),
        "a confirmed fresh opening must not carry the obsolete rename warning: {prompt}"
    );
}

#[test]
fn source_root_selection_restored_at_handoff_preserves_opening_and_user_text() {
    let label = "confirmation-late-restoration";
    let grant = "confirmation-late-restoration-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("confirmation-late-restoration-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    let original = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            original.generation,
        )
        .unwrap();
    let historical = "[TermAl] An earlier check was withheld because its opening was unconfirmed.";
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line(historical.to_owned())
    });
    let mut dispatch = claimed.dispatch();
    assert!(matches!(
        claimed.state.prepare_engram_turn_delivery_off_lock(
            &claimed.session_id,
            dispatch.engram_dispatch_generation().unwrap()
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    let opening = claimed.state.engram_turn_root_capture(&claimed.session_id);
    assert!(!matches!(
        opening,
        EngramRootCapture::Recorded {
            state: EngramSourceRootState::Named,
            ..
        }
    ));
    let warning = "Engram no longer confirms this claim's local source-root selection";
    let TurnDispatch::PersistentCodex { command, .. } = &mut dispatch else {
        panic!("Codex fixture");
    };
    command
        .prompt
        .push_str(&format!("\nUser quotation: {warning}"));
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        opening,
        "later naming must not relabel the original admitted opening"
    );
    assert!(matches!(
        handoff_prepared_turn_dispatch(&claimed.state, dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered
    ));
    let prompt = received_prompt(&claimed);
    assert!(prompt.contains(historical), "{prompt}");
    assert!(
        prompt.contains(&format!("User quotation: {warning}")),
        "{prompt}"
    );
    assert_eq!(
        prompt.matches(warning).count(),
        1,
        "only the user's quotation may remain after acknowledged restoration: {prompt}"
    );
}

#[test]
fn source_root_current_selection_loss_keeps_the_rename_warning() {
    let label = "confirmation-current-loss";
    let grant = "confirmation-current-loss-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("confirmation-current-loss-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    let original = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone().unwrap(),
        )
    });
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            original.generation,
        )
        .unwrap();
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let prompt = received_prompt(&claimed);
    assert!(
        prompt.contains("Engram no longer confirms this claim's local source-root selection"),
        "genuine current loss must remain visible: {prompt}"
    );
    assert!(!matches!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Recorded {
            state: EngramSourceRootState::Named,
            ..
        }
    ));
}

fn selection_loss_fixture(
    label: &str,
    replies: Vec<ScriptedEngramControlResponse>,
) -> (ClaimedRoot, PathBuf, EngramWorkSourceRoot) {
    let claimed = ClaimedRoot::new_scripted(label, replies);
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let (token, binding) = claimed.record(|record| {
        (
            record.engram.routing_token.clone().unwrap(),
            record.engram.work_binding.clone().unwrap(),
        )
    });
    let store = claimed_root_store(&claimed);
    let lost = engram_work_source_root_for_claim(
        &claimed.state.inner.lock().unwrap().engram_work_source_roots,
        &store,
        &binding.work_id,
        &binding.claim_id,
    )
    .unwrap()
    .clone();
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            named.generation,
        )
        .unwrap();
    (claimed, worktree, lost)
}

#[test]
fn source_root_selection_warning_survives_failed_send_and_old_acknowledgement() {
    let label = "confirmation-failed-send";
    let grant = "confirmation-failed-send-grant";
    let (claimed, _, lost) = selection_loss_fixture(
        label,
        vec![
            bind_reply("confirmation-failed-send-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let old = claimed.record(|record| record.engram.source_root_notices.clone());
    claimed.record(|record| {
        record
            .engram
            .queue_source_root_notice(EngramSourceRootNoticeKind::SelectionLoss {
                selection: lost,
            });
        let successor = record.engram.source_root_notices.clone();
        assert_ne!(successor, old);
        assert_eq!(successor[0].line(), old[0].line());
        acknowledge_engram_source_root_notices_locked(record, &old);
        assert_eq!(record.engram.source_root_notices, successor);
    });
    let dispatch = claimed.dispatch();
    assert!(matches!(
        claimed.state.prepare_engram_turn_delivery_off_lock(
            &claimed.session_id,
            dispatch.engram_dispatch_generation().unwrap()
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    let pending = claimed.record(|record| record.engram.source_root_notices.clone());
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    drop(claimed.runtime_rx);
    assert!(handoff_prepared_turn_dispatch(&state, dispatch).is_err());
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram
            .source_root_notices,
        pending,
        "failed send is not delivery"
    );
}

#[test]
fn source_root_selection_warning_returns_with_the_live_claim_after_an_unrelated_turn() {
    let label = "confirmation-return";
    let other_label = "confirmation-away";
    let away_grant = "confirmation-away-grant";
    let return_grant = "confirmation-return-grant";
    let (claimed, _, lost) = selection_loss_fixture(
        label,
        vec![
            bind_reply("confirmation-away-token"),
            grant_reply(away_grant),
            begin_reply(away_grant),
            checkpoint_reply(away_grant),
            bind_reply("confirmation-return-token"),
            grant_reply(return_grant),
            begin_reply(return_grant),
            checkpoint_reply(return_grant),
        ],
    );
    let original_binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let other_binding = test_control_work_binding(&format!("turn-observation-{other_label}"), 1);
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &other_binding);
    claimed.transport.work_bindings.lock().unwrap().clear();
    claimed
        .transport
        .work_bindings
        .lock()
        .unwrap()
        .push_back(Ok(Some(other_binding.clone())));
    claimed.record(|record| record.engram.work_binding = Some(other_binding.clone()));
    let pending = claimed.record(|record| record.engram.source_root_notices.clone());
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let away_prompt = received_prompt(&claimed);
    assert!(
        !away_prompt.contains("Engram no longer confirms this claim's local source-root selection"),
        "another work must not receive the rename instruction: {away_prompt}"
    );
    claimed.record(|record| {
        assert_eq!(
            record
                .engram
                .active_turn_source_binding
                .as_ref()
                .unwrap()
                .claim_id,
            other_binding.claim_id
        );
        assert_eq!(
            record.engram.source_root_notices, pending,
            "a different association suppresses, but never consumes, the live notice"
        );
    });
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .unwrap();
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &original_binding);
    claimed
        .transport
        .work_bindings
        .lock()
        .unwrap()
        .push_back(Ok(Some(original_binding.clone())));
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let return_prompt = received_prompt(&claimed);
    assert!(
        return_prompt
            .contains("Engram no longer confirms this claim's local source-root selection"),
        "the still-live lost selection must be visible on return: {return_prompt}"
    );
    claimed.record(|record| {
        assert_eq!(
            record
                .engram
                .active_turn_source_binding
                .as_ref()
                .unwrap()
                .claim_id,
            lost.claim_id
        );
        assert!(
            record.engram.source_root_notices.is_empty(),
            "accepted send consumes its instance"
        );
    });
}

#[test]
fn source_root_unacknowledged_restoration_cannot_retire_the_selection_warning() {
    let label = "confirmation-unacknowledged";
    let grant = "confirmation-unacknowledged-grant";
    let (claimed, worktree, lost) = selection_loss_fixture(
        label,
        vec![
            bind_reply("confirmation-unacknowledged-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let dispatch = claimed.dispatch();
    assert!(matches!(
        claimed.state.prepare_engram_turn_delivery_off_lock(
            &claimed.session_id,
            dispatch.engram_dispatch_generation().unwrap()
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    let opening = claimed.record(|record| record.engram.active_turn_root_capture.clone());
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    );
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(
        result.is_err(),
        "an unacknowledged name cannot claim success"
    );
    assert!(engram_authority_work_unresolved(
        &claimed.state.inner.lock().unwrap(),
        &lost.store,
        &lost.work_id
    ));
    assert!(matches!(
        handoff_prepared_turn_dispatch(&claimed.state, dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered
    ));
    let prompt = received_prompt(&claimed);
    assert!(
        prompt.contains("Engram no longer confirms this claim's local source-root selection"),
        "unacknowledged restoration is not confirmation: {prompt}"
    );
    claimed.record(|record| {
        assert_eq!(record.engram.active_turn_root_capture, opening);
    });
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed,
        "pending authority withholds evidence without rewriting the admitted opening"
    );
}

#[test]
fn source_root_selection_warning_isolates_store_work_and_claim_scopes() {
    let grant = "confirmation-scope-grant";
    let (claimed, _, lost) = selection_loss_fixture(
        "confirmation-scope",
        vec![
            bind_reply("confirmation-scope-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let mut other_store = lost.clone();
    other_store.store.project_id.push_str("-other");
    let mut other_work = lost.clone();
    other_work.work_id.push_str("-other");
    let mut other_claim = lost.clone();
    other_claim.claim_id.push_str("-other");
    let foreign = claimed.record(|record| {
        for selection in [other_store, other_work, other_claim] {
            record
                .engram
                .queue_source_root_notice(EngramSourceRootNoticeKind::SelectionLoss { selection });
        }
        assert_eq!(record.engram.source_root_notices.len(), 4);
        record.engram.source_root_notices[1..].to_vec()
    });
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let prompt = received_prompt(&claimed);
    assert_eq!(
        prompt
            .matches("Engram no longer confirms this claim's local source-root selection")
            .count(),
        1,
        "only this admitted association receives its warning: {prompt}"
    );
    claimed.record(|record| {
        assert_eq!(record.engram.source_root_notices, foreign);
    });
}

#[test]
fn source_root_successful_explicit_clear_supersedes_the_loss_instruction() {
    let label = "confirmation-clear";
    let grant = "confirmation-clear-grant";
    let (claimed, _, lost) = selection_loss_fixture(
        label,
        vec![
            bind_reply("confirmation-clear-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    // Make status agree with the canonical Ended read already reconciled by
    // the fixture. Clearing the absent selection must issue no new event.
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .latest
        .get_mut(&lost.claim_id)
        .unwrap()["kind"] = json!("ended");
    let dispatch = claimed.dispatch();
    assert!(matches!(
        claimed.state.prepare_engram_turn_delivery_off_lock(
            &claimed.session_id,
            dispatch.engram_dispatch_generation().unwrap()
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    let opening = claimed.record(|record| record.engram.active_turn_root_capture.clone());
    let before = claimed.transport.requests.lock().unwrap().len();
    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    assert_eq!(cleared.generation, 0);
    assert!(cleared.root.is_none());
    assert!(
        claimed.transport.requests.lock().unwrap()[before..]
            .iter()
            .all(|request| request.request["operation"] != "named_root_bind")
    );
    handoff_prepared_turn_dispatch(&claimed.state, dispatch).unwrap();
    let prompt = received_prompt(&claimed);
    assert!(
        !prompt.contains("Engram no longer confirms this claim's local source-root selection"),
        "an acknowledged intentional clear supersedes the old instruction: {prompt}"
    );
    claimed.record(|record| {
        assert_eq!(record.engram.active_turn_root_capture, opening);
    });
}

#[test]
fn source_root_rejected_or_unacknowledged_clear_keeps_the_loss_instruction() {
    let label = "confirmation-clear-failure";
    let grant = "confirmation-clear-failure-grant";
    let (claimed, _, lost) = selection_loss_fixture(
        label,
        vec![
            bind_reply("confirmation-clear-failure-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .latest
        .get_mut(&lost.claim_id)
        .unwrap()["kind"] = json!("ended");
    let pending = claimed.record(|record| record.engram.source_root_notices.clone());
    let rejected = claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: format!("w-{label}"),
                path: Some(None),
            },
        )
        .unwrap_err();
    assert!(rejected.message.contains("`path` is null"));
    claimed.record(|record| assert_eq!(record.engram.source_root_notices, pending));
    let dispatch = claimed.dispatch();
    assert!(matches!(
        claimed.state.prepare_engram_turn_delivery_off_lock(
            &claimed.session_id,
            dispatch.engram_dispatch_generation().unwrap(),
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)]);
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(cleared.is_err());
    claimed.record(|record| assert_eq!(record.engram.source_root_notices, pending));
    handoff_prepared_turn_dispatch(&claimed.state, dispatch).unwrap();
    assert!(
        received_prompt(&claimed)
            .contains("Engram no longer confirms this claim's local source-root selection")
    );
    assert_eq!(
        claimed.state.engram_turn_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
}

#[test]
fn source_root_explicit_clear_cannot_consume_a_later_successor_or_foreign_notice() {
    let label = "confirmation-clear-successor";
    let grant = "confirmation-clear-successor-grant";
    let (claimed, _, lost) = selection_loss_fixture(
        label,
        vec![
            bind_reply("confirmation-clear-successor-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .latest
        .get_mut(&lost.claim_id)
        .unwrap()["kind"] = json!("ended");
    let original = claimed.record(|record| record.engram.source_root_notices[0].clone());
    let mut foreign = lost.clone();
    foreign.claim_id.push_str("-other");
    let foreign = claimed.record(|record| {
        record
            .engram
            .queue_source_root_notice(EngramSourceRootNoticeKind::SelectionLoss {
                selection: foreign,
            });
        record.engram.source_root_notices[1].clone()
    });
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.queue_source_root_notice(
                EngramSourceRootNoticeKind::SelectionLoss { selection: lost },
            );
        }));
    });
    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    assert_eq!(cleared.generation, 0);
    let successor = claimed.record(|record| {
        assert_eq!(record.engram.source_root_notices.len(), 2);
        assert!(record.engram.source_root_notices.contains(&foreign));
        let successor = record
            .engram
            .source_root_notices
            .iter()
            .find(|notice| notice.scope() == original.scope())
            .unwrap()
            .clone();
        assert_ne!(successor.id, original.id);
        assert_eq!(successor.line(), original.line());
        successor
    });
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    assert!(received_prompt(&claimed).contains(successor.line()));
    claimed.record(|record| assert_eq!(record.engram.source_root_notices, vec![foreign]));
}
