// Ordinary naming recovery replaces the retired control-rebind oracles.
// Each case preserves the old safety purpose (reload, exact claim, failed
// save, competing owner, immutable retry, refusal custody) while proving
// that no control token, bind key or old grant becomes ordinary authority.
use super::*;

fn binding_state(turn: &EOCheckedTurn)
    -> (Option<String>, bool, Option<EngramControlWorkBinding>)
{
    turn.record(|record| (
        record.engram.historical_control.get("engramRoutingToken").and_then(Value::as_str).map(str::to_owned),
        record.engram.historical_control.get("engramRebindRequired").and_then(Value::as_bool).unwrap_or(false),
        record.engram.work_binding.clone(),
    ))
}

fn ordinary_root(label: &str) -> (EOCheckedTurn, PathBuf) {
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = claimed.root.join(".worktrees").join("ordinary-naming");
    run_git_test_command(&claimed.root, &[
        "worktree", "add", "--quiet", "-b", "ordinary-naming", worktree.to_str().unwrap(),
    ]);
    claimed.record_mut(|record| record.engram.work_binding = None);
    (claimed, worktree)
}

fn reload_naming_session(claimed: &EOCheckedTurn) {
    claimed.record_mut(|record| {
        let saved = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
        let loaded: PersistedSessionRecord = serde_json::from_value(saved).unwrap();
        *record = loaded.into_record().unwrap();
    });
}

fn history(claimed: &EOCheckedTurn, token: &str, flagged: bool) {
    claimed.record_mut(|record| {
        record.engram.historical_control.insert("engramRoutingToken".into(), json!(token));
        record.engram.historical_control.insert("engramRebindRequired".into(), json!(flagged));
    });
}

fn ordinary_naming_request(turn: &EOCheckedTurn, path: Option<&FsPath>)
    -> Result<EngramSourceRootResponse, ApiError>
{
    // The association comes from the explicit shared fixture, never old credentials.
    let work = {
        let roots = turn.transport.named_roots.lock().unwrap();
        let bindings = &roots.as_ref().unwrap().read_bindings;
        assert_eq!(bindings.len(), 1);
        bindings.values().next().unwrap().work_id.clone()
    };
    turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work, path: path.map(|path| Some(path.to_string_lossy().into_owned())),
    })
}

fn reload_legacy_control_setting(turn: &EOCheckedTurn) {
    let mut inner = turn.state.inner.lock().unwrap();
    let project = inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
        .session.project_id.clone().unwrap();
    let settings = inner.projects.iter_mut().find(|item| item.id == project).unwrap()
        .engram.as_mut().unwrap();
    let enabled = settings.execution_observability;
    let mut saved = serde_json::to_value(&*settings).unwrap();
    saved["turnGatedControl"] = json!(true);
    *settings = serde_json::from_value(saved).unwrap();
    assert_eq!(settings.execution_observability, enabled);
    assert!(serde_json::to_value(&*settings).unwrap().get("turnGatedControl").is_none(),
        "the retired setting is ignored, never an executable authority field");
}

fn roots<T>(claimed: &EOCheckedTurn, change: impl FnOnce(&mut TestEngramNamedRoots) -> T) -> T {
    change(claimed.transport.named_roots.lock().unwrap().as_mut().unwrap())
}

fn operations(claimed: &EOCheckedTurn, operation: &str) -> Vec<Value> {
    claimed.transport.requests().into_iter()
        .filter(|request| request.request["operation"] == operation)
        .map(|request| request.request).collect()
}

fn assert_no_control_wire(claimed: &EOCheckedTurn) {
    assert!(claimed.transport.requests().iter().all(|request| !matches!(
        request.request["operation"].as_str(),
        Some("session_bind" | "session_status" | "turn_evaluate" |
            "turn_begin" | "turn_grant_read" | "turn_checkpoint")
    )));
    assert!(operations(claimed, "named_root_bind").iter()
        .all(|request| request.get("routing_token").is_none()));
}

#[test]
fn reload_preserves_historical_token_while_naming_relearns_the_exact_claim() {
    let label = "ordinary-reload-history";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "historical-token", false);
    let first = ordinary_naming_request(&claimed, Some(&worktree)).unwrap();
    reload_naming_session(&claimed);
    assert_eq!(binding_state(&claimed), (Some("historical-token".into()), false, None));
    let again = ordinary_naming_request(&claimed, Some(&worktree)).unwrap();
    assert_eq!(again.root, first.root);
    assert_eq!(again.generation, first.generation);
    assert_eq!(binding_state(&claimed), (Some("historical-token".into()), false,
        roots(&claimed, |model| model.read_bindings.values().next().cloned())));
    reload_naming_session(&claimed);
    assert_eq!(binding_state(&claimed), (Some("historical-token".into()), false, None));
    assert_no_control_wire(&claimed);
}

#[test]
fn unflagged_historical_token_is_not_used_as_an_ordinary_naming_credential() {
    let label = "ordinary-unflagged-history";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "historical-token", false);
    let first = ordinary_naming_request(&claimed, Some(&worktree)).unwrap();
    let before = binding_state(&claimed);
    let again = ordinary_naming_request(&claimed, Some(&worktree)).unwrap();
    assert_eq!(again.generation, first.generation);
    assert_eq!(binding_state(&claimed), before);
    assert_no_control_wire(&claimed);
}

#[test]
fn saved_legacy_setting_true_cannot_restore_control_naming_or_token_authority() {
    let label = "ordinary-legacy-setting";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "flagged-history", true);
    reload_legacy_control_setting(&claimed);
    let named = ordinary_naming_request(&claimed, Some(&worktree)).unwrap();
    assert!(named.root.is_some());
    assert_eq!(binding_state(&claimed).0.as_deref(), Some("flagged-history"));
    assert!(binding_state(&claimed).1);
    assert_no_control_wire(&claimed);
}

#[test]
fn ordinary_naming_preserves_unresolved_control_history_without_settling_it() {
    for case in ["active", "uncertain", "legacy", "checkpoint", "binding", "retained-bind"] {
        let label = format!("ordinary-history-{case}");
        let (claimed, worktree) = ordinary_root(&label);
        history(&claimed, "flagged-history", true);
        let key = match case {
            "active" => "engramActiveGrantId",
            "uncertain" => "engramUncertainGrantId",
            "legacy" => "engramBeginsRecorded",
            "checkpoint" => "engramCheckpointInProgress",
            "binding" => "engramBindInProgress",
            "retained-bind" => "engramRetainedBind",
            _ => unreachable!(),
        };
        let raw = match case {
            "active" => json!("keep-active"),
            "uncertain" => json!("keep-uncertain"),
            "legacy" => json!(false),
            "checkpoint" | "binding" => json!(true),
            "retained-bind" => json!({
                "request": {"operation":"session_status", "routing_token":"flagged-history"},
                "unknown":{"future":[null, true, "whole żółw 🐢 payload"]},
            }),
            _ => unreachable!(),
        };
        claimed.record_mut(|record| {
            record.engram.historical_control.insert(key.to_owned(), raw.clone());
        });
        let evidence = || claimed.record(|record| record.engram.historical_control.clone());
        let before = evidence();
        assert!(ordinary_naming_request(&claimed, Some(&worktree)).unwrap().root.is_some());
        assert_eq!(evidence(), before, "{case}: ordinary naming cannot settle control history");
        assert_eq!(binding_state(&claimed).0.as_deref(), Some("flagged-history"));
        assert!(binding_state(&claimed).1);
        assert_no_control_wire(&claimed);
    }
}

#[test]
fn failed_claim_association_save_restores_history_and_releases_the_naming_owner() {
    let label = "ordinary-association-save-fails";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "flagged-history", true);
    let before = binding_state(&claimed);
    TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(|fails| fails.set(true));
    let result = ordinary_naming_request(&claimed, Some(&worktree));
    TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(|fails| fails.set(false));
    let error = result.expect_err("a failed association save must refuse naming");
    assert!(error.message.contains("persisting naming binding"), "{error:?}");
    assert_eq!(binding_state(&claimed), before);
    assert!(operations(&claimed, "named_root_bind").is_empty(), "no write before durable association");
    claimed.record(|record| assert!(record.engram.source_root_bind_owner.is_none()));
    assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.is_empty());
    assert_no_control_wire(&claimed);
}

#[test]
fn a_competing_work_association_is_not_overwritten_by_naming() {
    let label = "ordinary-competing-association";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "flagged-history", true);
    let replacement = test_control_work_binding("competing-association", 1);
    let expected = replacement.clone();
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.work_binding = Some(replacement);
        }));
    });
    let result = ordinary_naming_request(&claimed, Some(&worktree));
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| hook.borrow_mut().take());
    assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
    assert_eq!(binding_state(&claimed), (Some("flagged-history".into()), true, Some(expected)));
    assert!(operations(&claimed, "named_root_bind").is_empty());
    claimed.record(|record| assert!(record.engram.source_root_bind_owner.is_none()));
    assert_no_control_wire(&claimed);
}

#[test]
fn retry_after_failed_association_save_does_not_replay_or_mint_a_control_binding() {
    let label = "ordinary-association-retry";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "another-owners-history", true);
    let before = binding_state(&claimed);
    TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(|fails| fails.set(true));
    let failed = ordinary_naming_request(&claimed, Some(&worktree));
    TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(|fails| fails.set(false));
    assert!(failed.is_err());
    assert_eq!(binding_state(&claimed), before);
    let named = ordinary_naming_request(&claimed, Some(&worktree)).unwrap();
    assert!(named.root.is_some());
    assert_eq!(operations(&claimed, "named_root_bind").len(), 1);
    assert_eq!(roots(&claimed, |model| model.seen.len()), 1);
    assert_eq!(binding_state(&claimed).0, before.0);
    assert!(binding_state(&claimed).1);
    assert_no_control_wire(&claimed);
}

#[test]
fn a_passive_inactive_control_marker_does_not_disable_the_ordinary_work_child() {
    let label = "ordinary-inactive-history";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "inactive-token-history", true);
    claimed.record_mut(|record| {
        record.engram.historical_control.insert("engramSessionStatus".into(),
            json!({"phase":"exited", "routing_token":"inactive-token-history", "future":[null,true]}));
    });
    let historical = claimed.record(|record| record.engram.historical_control.clone());
    assert!(ordinary_naming_request(&claimed, Some(&worktree)).unwrap().root.is_some());
    claimed.record(|record| assert_eq!(record.engram.historical_control, historical,
        "naming neither interprets nor settles a passive inactive control marker"));
    assert_eq!(binding_state(&claimed).0.as_deref(), Some("inactive-token-history"));
    assert!(binding_state(&claimed).1);
    assert_no_control_wire(&claimed);
}

#[test]
fn canonical_named_root_refusal_keeps_its_reason_and_pending_intent_without_rebinding() {
    let label = "ordinary-root-refusal";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "history", false);
    roots(&claimed, |model| model.refuse_next = true);
    let error = ordinary_naming_request(&claimed, Some(&worktree)).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("named_root_binding_refused"), "{error:?}");
    assert_eq!(binding_state(&claimed).0.as_deref(), Some("history"));
    assert!(!binding_state(&claimed).1);
    assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.is_empty());
    assert!(claimed.state.inner.lock().unwrap().engram_named_root_journal.iter()
        .any(|entry| entry.pending.is_some()), "the refused intent remains unconfirmed");
    assert_no_control_wire(&claimed);
}

#[test]
fn a_concurrent_historical_token_change_is_preserved_without_changing_work_authority() {
    let label = "ordinary-token-changed";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "old-history", true);
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.historical_control.insert("engramRoutingToken".into(), json!("other-owner-history"));
        }));
    });
    let result = ordinary_naming_request(&claimed, Some(&worktree));
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| hook.borrow_mut().take());
    assert!(result.unwrap().root.is_some());
    assert_eq!(binding_state(&claimed).0.as_deref(), Some("other-owner-history"));
    assert!(binding_state(&claimed).1);
    assert_no_control_wire(&claimed);
}

#[test]
fn refusal_with_legacy_true_preserves_control_evidence_and_retries_the_exact_root_intent() {
    let label = "ordinary-legacy-refusal";
    let (claimed, worktree) = ordinary_root(label);
    history(&claimed, "keep-history", true);
    reload_legacy_control_setting(&claimed);
    roots(&claimed, |model| model.refuse_next = true);
    let failed = ordinary_naming_request(&claimed, Some(&worktree)).unwrap_err();
    assert!(failed.message.contains("named_root_binding_refused"), "{failed:?}");
    assert_eq!(binding_state(&claimed).0.as_deref(), Some("keep-history"));
    assert!(binding_state(&claimed).1);
    let named = ordinary_naming_request(&claimed, Some(&worktree)).unwrap();
    assert!(named.root.is_some());
    let writes = operations(&claimed, "named_root_bind");
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0], writes[1], "same immutable root request and idempotency key");
    assert_eq!(roots(&claimed, |model| model.seen.len()), 1);
    assert_no_control_wire(&claimed);
}
