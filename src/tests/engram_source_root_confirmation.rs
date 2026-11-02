//! Selection-loss notices follow actual ordinary provider delivery and naming ACKs.
use super::*;

const SELECTION_WARNING: &str = "Engram no longer confirms this claim's local source-root selection";

fn confirmation_root(label: &str) -> (EOCheckedTurn, PathBuf) {
    let turn = EOCheckedTurn::idle(label, true);
    prepare_naming_worktree_container(&turn.root);
    let path = turn.root.join(".worktrees").join("confirmation");
    run_git_test_command(&turn.root, &[
        "worktree", "add", "--quiet", "-b", "confirmation", path.to_str().unwrap(),
    ]);
    (turn, path)
}

fn confirmation_name(turn: &EOCheckedTurn, path: Option<&FsPath>)
    -> Result<EngramSourceRootResponse, ApiError>
{
    let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work, path: path.map(|path| Some(path.to_string_lossy().into_owned())),
    })
}

// Keep the original schedule: the opening is prepared before the failed
// name, and its warning is acknowledged only by the later provider handoff.
fn prepare_confirmation_opening(turn: &EOCheckedTurn, text: &str) -> TurnDispatch {
    let dispatch = match turn.state.dispatch_turn(&turn.session_id, SendMessageRequest {
        text: text.to_owned(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the idle ordinary root must dispatch"),
    };
    let (token, generation) = turn.record(|record|
        (record.runtime.runtime_token().unwrap(), record.active_turn_generation));
    turn.state.prepare_ordinary_opening_root_before_handoff(&turn.session_id, &token, generation);
    turn.record(|record| assert!(record.engram.unadmitted_active_turn.as_ref().unwrap()
        .opening_prepared));
    dispatch
}

fn confirmation_store(turn: &EOCheckedTurn) -> EngramAuthorityStoreKey {
    let inner = turn.state.inner.lock().unwrap();
    engram_project_for_session_locked(&inner, &turn.session_id).unwrap()
        .engram.as_ref().unwrap().authority_store_key.clone().unwrap()
}

fn confirmation_read(turn: &EOCheckedTurn, state: EngramNamedRootState) {
    let (target, fence) = {
        let inner = turn.state.inner.lock().unwrap();
        (AppState::engram_source_root_target_locked(&inner, &turn.session_id).unwrap().unwrap(),
            EngramRootReadFence::capture(&inner))
    };
    turn.state.reconcile_engram_named_root_with_route_classified_until(
        &turn.session_id, target.work_binding.as_ref(), Some(state), &fence,
        Some(&target.connection), turn.state.engram_budget_clock().now() + Duration::from_secs(2),
    ).unwrap_or_else(|failure| panic!("canonical root reconciliation failed: {}", failure.into_transport()));
}

// Change the canonical producer feed, then use the actual fenced reader.
// No local selection is removed by this fixture.
fn confirmation_end_selection(turn: &EOCheckedTurn) {
    let binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    {
        let mut roots = turn.transport.named_roots.lock().unwrap();
        let roots = roots.as_mut().unwrap();
        let event = roots.latest.get_mut(&binding.claim_id).unwrap();
        let next = event["position"]["position"].as_i64().unwrap() + 1;
        event["kind"] = json!("ended");
        event["event"] = json!(sha256_hex(format!("selection-ended-{next}").as_bytes()));
        event["position"]["position"] = json!(next);
    }
    confirmation_read(turn, EngramNamedRootState::None);
}

fn selection_loss_fixture(label: &str) -> (EOCheckedTurn, PathBuf, EngramWorkSourceRoot) {
    let (turn, path) = confirmation_root(label);
    confirmation_name(&turn, Some(&path)).unwrap();
    let binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let store = confirmation_store(&turn);
    let lost = engram_work_source_root_for_claim(
        &turn.state.inner.lock().unwrap().engram_work_source_roots,
        &store, &binding.work_id, &binding.claim_id,
    ).unwrap().clone();
    confirmation_end_selection(&turn);
    (turn, path, lost)
}

#[test]
fn source_root_restored_selection_does_not_deliver_an_obsolete_rename_warning() {
    let (turn, path) = confirmation_root("confirmation-restored-selection");
    let original = confirmation_name(&turn, Some(&path)).unwrap();
    let bound = turn.record(|record| record.engram.named_root.clone().unwrap());
    confirmation_read(&turn, bound);
    assert!(!turn.record(|record| record.engram.pending_source_root_line.clone())
        .unwrap_or_default().contains(SELECTION_WARNING));
    let binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let store = confirmation_store(&turn);
    confirmation_end_selection(&turn);
    assert!(turn.record(|record| record.engram.source_root_notices.iter()
        .any(|notice| notice.line().contains(SELECTION_WARNING))));
    assert!(engram_work_source_root_for_claim(
        &turn.state.inner.lock().unwrap().engram_work_source_roots,
        &store, &binding.work_id, &binding.claim_id,
    ).is_none());
    let restored = confirmation_name(&turn, Some(&path)).unwrap();
    assert!(restored.generation > original.generation);
    let kept = confirmation_name(&turn, Some(&path)).unwrap();
    assert_eq!(kept.generation, restored.generation);
    {
        let inner = turn.state.inner.lock().unwrap();
        let selected = engram_work_source_root_for_claim(
            &inner.engram_work_source_roots, &store, &binding.work_id, &binding.claim_id,
        ).unwrap();
        assert_eq!(selected.generation, restored.generation);
        assert_eq!(Some(&selected.root), restored.root.as_ref());
        assert!(!engram_authority_work_unresolved(&inner, &store, &binding.work_id));
    }
    let prompt = turn.begin_prompt("Inspect the restored selection.").prompt;
    assert!(!prompt.contains(SELECTION_WARNING), "{prompt}");
    turn.record(|record| {
        let opening = record.engram.observed_opening().unwrap();
        assert!(matches!(opening.provenance.clone(), EngramRootCapture::Recorded {
            state: EngramSourceRootState::Named, generation, ..
        } if generation == restored.generation as i64));
        assert_eq!(record.engram.observed_source_root().unwrap().generation, restored.generation);
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn source_root_restoration_preserves_an_earlier_opening_and_later_user_text() {
    let (turn, path, _) = selection_loss_fixture("confirmation-late-restoration");
    let historical = "[TermAl] An earlier observation had an unconfirmed opening.";
    turn.record_mut(|record| record.engram.set_pending_source_root_line(historical.to_owned()));
    let prompt = turn.begin_prompt("Inspect the lost selection.").prompt;
    assert!(prompt.contains(historical), "{prompt}");
    assert!(prompt.contains(SELECTION_WARNING), "{prompt}");
    let original = turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert!(!matches!(original.opening_root.as_ref().unwrap().provenance.clone(),
        EngramRootCapture::Recorded { state: EngramSourceRootState::Named, .. }));
    confirmation_name(&turn, Some(&path)).unwrap();
    turn.record(|record| {
        let current = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(current.opening_root, original.opening_root);
        assert_eq!(current.host_turn_ref, original.host_turn_ref);
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    let prompt = turn.begin_prompt(&format!("User quotation: {SELECTION_WARNING}")).prompt;
    assert!(prompt.contains(&format!("User quotation: {SELECTION_WARNING}")), "{prompt}");
    assert_eq!(prompt.matches(SELECTION_WARNING).count(), 1, "{prompt}");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn source_root_current_selection_loss_keeps_the_rename_warning() {
    let (turn, _, _) = selection_loss_fixture("confirmation-current-loss");
    let prompt = turn.begin_prompt("Inspect the missing selection.").prompt;
    assert!(prompt.contains(SELECTION_WARNING), "{prompt}");
    assert!(!matches!(turn.record(|record| record.engram.observed_opening().unwrap().provenance.clone()),
        EngramRootCapture::Recorded { state: EngramSourceRootState::Named, .. }));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn source_root_selection_warning_survives_failed_send_and_old_acknowledgement() {
    let (mut turn, _, lost) = selection_loss_fixture("confirmation-failed-send");
    let old = turn.record(|record| record.engram.source_root_notices.clone());
    turn.record_mut(|record| {
        record.engram.queue_source_root_notice(EngramSourceRootNoticeKind::SelectionLoss { selection: lost });
        let successor = record.engram.source_root_notices.clone();
        assert_ne!(successor, old);
        assert_eq!(successor[0].line(), old[0].line());
        acknowledge_engram_source_root_notices_locked(record, &old);
        assert_eq!(record.engram.source_root_notices, successor);
    });
    let dispatch = match turn.state.dispatch_turn(&turn.session_id, SendMessageRequest {
        text: "Inspect the lost selection.".to_owned(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the real idle owner must dispatch"),
    };
    let pending = turn.record(|record| record.engram.source_root_notices.clone());
    let (_unused_sender, receiver) = mpsc::channel();
    turn._runtime_rx = Arc::new(Mutex::new(receiver));
    assert!(deliver_turn_dispatch(&turn.state, dispatch).is_err());
    turn.record(|record| assert_eq!(record.engram.source_root_notices, pending, "failed send is not delivery"));
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn source_root_selection_warning_returns_with_the_live_claim_after_an_unrelated_turn() {
    let (turn, _, lost) = selection_loss_fixture("confirmation-return");
    let original = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let other = test_control_work_binding("confirmation-away", 1);
    turn.transport.enable_named_roots(&turn.session_id, &other);
    turn.transport.set_current_work_binding(Some(other.clone()));
    turn.record_mut(|record| record.engram.work_binding = Some(other.clone()));
    let pending = turn.record(|record| record.engram.source_root_notices.clone());
    let away = turn.begin_prompt("An unrelated work.").prompt;
    assert!(!away.contains(SELECTION_WARNING), "{away}");
    turn.record(|record| {
        assert_eq!(record.engram.observed_opening().unwrap().binding.claim_id, other.claim_id);
        assert_eq!(record.engram.source_root_notices, pending, "suppressed is not consumed");
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.transport.enable_named_roots(&turn.session_id, &original);
    turn.transport.set_current_work_binding(Some(original.clone()));
    turn.record_mut(|record| record.engram.work_binding = Some(original));
    let returned = turn.begin_prompt("Return to the live claim.").prompt;
    assert!(returned.contains(SELECTION_WARNING), "{returned}");
    turn.record(|record| {
        assert_eq!(record.engram.observed_opening().unwrap().binding.claim_id, lost.claim_id);
        assert!(record.engram.source_root_notices.is_empty());
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn source_root_unacknowledged_restoration_cannot_retire_the_selection_warning() {
    let (turn, path, lost) = selection_loss_fixture("confirmation-unacknowledged");
    let dispatch = prepare_confirmation_opening(&turn, "Inspect the unacknowledged restoration.");
    let opening = turn.record(|record| record.engram.observed_opening().unwrap().clone());
    let pending = turn.record(|record| record.engram.source_root_notices.clone());
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = confirmation_name(&turn, Some(&path));
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(result.is_err(), "an unacknowledged name cannot claim success");
    assert!(engram_authority_work_unresolved(&turn.state.inner.lock().unwrap(), &lost.store, &lost.work_id));
    turn.record(|record| assert_eq!(record.engram.source_root_notices, pending));
    deliver_turn_dispatch(&turn.state, dispatch).unwrap();
    let prompt = delivered_naming_prompt(&turn);
    assert!(prompt.contains(SELECTION_WARNING), "{prompt}");
    turn.record(|record| assert_eq!(record.engram.observed_opening(), Some(&opening),
        "failed naming cannot rewrite the already prepared ordinary opening"));
    assert_eq!(turn.state.engram_turn_root_capture(&turn.session_id),
        EngramRootCapture::Unconfirmed);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn source_root_selection_warning_isolates_store_work_and_claim_scopes() {
    let (turn, _, lost) = selection_loss_fixture("confirmation-scope");
    let mut other_store = lost.clone();
    other_store.store.project_id.push_str("-other");
    let mut other_work = lost.clone();
    other_work.work_id.push_str("-other");
    let mut other_claim = lost.clone();
    other_claim.claim_id.push_str("-other");
    let foreign = turn.record_mut(|record| {
        for selection in [other_store, other_work, other_claim] {
            record.engram.queue_source_root_notice(EngramSourceRootNoticeKind::SelectionLoss { selection });
        }
        assert_eq!(record.engram.source_root_notices.len(), 4);
        record.engram.source_root_notices[1..].to_vec()
    });
    let prompt = turn.begin_prompt("Inspect this claim.").prompt;
    assert_eq!(prompt.matches(SELECTION_WARNING).count(), 1, "{prompt}");
    turn.record(|record| assert_eq!(record.engram.source_root_notices, foreign));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn source_root_successful_explicit_clear_supersedes_the_loss_instruction() {
    let (turn, _, _) = selection_loss_fixture("confirmation-clear");
    let before = turn.transport.requests().len();
    let cleared = confirmation_name(&turn, None).unwrap();
    assert_eq!(cleared.generation, 0);
    assert!(cleared.root.is_none());
    assert!(turn.transport.requests()[before..].iter()
        .all(|request| request.request["operation"] != "named_root_bind"));
    let prompt = turn.begin_prompt("Inspect the intentional clear.").prompt;
    assert!(!prompt.contains(SELECTION_WARNING), "{prompt}");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn source_root_rejected_or_unacknowledged_clear_keeps_the_loss_instruction() {
    let (turn, _, _) = selection_loss_fixture("confirmation-clear-failure");
    let pending = turn.record(|record| record.engram.source_root_notices.clone());
    let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    let error = turn.state.name_engram_source_root(&turn.session_id,
        EngramSourceRootRequest { work, path: Some(None) }).unwrap_err();
    assert!(error.message.contains("`path` is null"));
    turn.record(|record| assert_eq!(record.engram.source_root_notices, pending));
    let dispatch = prepare_confirmation_opening(&turn, "Inspect the failed clear.");
    let opening = turn.record(|record| record.engram.observed_opening().unwrap().clone());
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let cleared = confirmation_name(&turn, None);
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(cleared.is_err());
    turn.record(|record| assert_eq!(record.engram.source_root_notices, pending));
    deliver_turn_dispatch(&turn.state, dispatch).unwrap();
    let prompt = delivered_naming_prompt(&turn);
    assert!(prompt.contains(SELECTION_WARNING), "{prompt}");
    turn.record(|record| assert_eq!(record.engram.observed_opening(), Some(&opening),
        "failed clearing cannot rewrite the already prepared ordinary opening"));
    assert_eq!(turn.state.engram_turn_root_capture(&turn.session_id),
        EngramRootCapture::Unconfirmed);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn source_root_explicit_clear_cannot_consume_a_later_successor_or_foreign_notice() {
    let (turn, _, lost) = selection_loss_fixture("confirmation-clear-successor");
    let original = turn.record(|record| record.engram.source_root_notices[0].clone());
    let mut foreign = lost.clone();
    foreign.claim_id.push_str("-other");
    let foreign = turn.record_mut(|record| {
        record.engram.queue_source_root_notice(EngramSourceRootNoticeKind::SelectionLoss { selection: foreign });
        record.engram.source_root_notices[1].clone()
    });
    let state = turn.state.clone();
    let session = turn.session_id.clone();
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.queue_source_root_notice(
                EngramSourceRootNoticeKind::SelectionLoss { selection: lost });
        }));
    });
    let cleared = confirmation_name(&turn, None).unwrap();
    assert_eq!(cleared.generation, 0);
    let successor = turn.record(|record| {
        assert_eq!(record.engram.source_root_notices.len(), 2);
        assert!(record.engram.source_root_notices.contains(&foreign));
        let successor = record.engram.source_root_notices.iter()
            .find(|notice| notice.scope() == original.scope()).unwrap().clone();
        assert_ne!(successor.id, original.id);
        assert_eq!(successor.line(), original.line());
        successor
    });
    let prompt = turn.begin_prompt("Inspect the successor notice.").prompt;
    assert!(prompt.contains(successor.line()), "{prompt}");
    turn.record(|record| assert_eq!(record.engram.source_root_notices, vec![foreign]));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}
