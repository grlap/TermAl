// Owns the tests of source-root naming re-establishing a turn-gated-control
// OFF session's Base binding after a host reload flagged its restored token
// for rebinding: the naming bind it sends and the key it sends it under, what
// it adopts, the fences that refuse it, the rollback of a failed save, a
// concurrent replacement, and a token another binder replaced meanwhile. Also
// owns the flag a named-root refusal for an inactive control session sets,
// and the remedy each such refusal names.
// Does not own fresh tokenless naming (engram_source_root_naming.rs, whose
// fixture helpers this child module uses) or the verification-record sender.
// New module.
use super::*;

/// The session's binding fields that naming owns.
fn binding_state(
    claimed: &ClaimedRoot,
) -> (Option<String>, bool, Option<EngramControlWorkBinding>) {
    claimed.record(|record| {
        (
            record.engram.routing_token.clone(),
            record.engram.rebind_required,
            record.engram.work_binding.clone(),
        )
    })
}

fn sent_binds(claimed: &ClaimedRoot) -> Vec<RecordedEngramControlRequest> {
    claimed
        .transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "session_bind")
        .collect()
}

/// A host reload of the naming session: its record goes through the
/// persisted form and back, as a restart restores it.
fn reload_naming_session(claimed: &ClaimedRoot) {
    claimed.record(|record| {
        let saved = serde_json::to_value(PersistedSessionRecord::from_record(record))
            .expect("the record serializes");
        let loaded: PersistedSessionRecord =
            serde_json::from_value(saved).expect("the record loads");
        *record = loaded.into_record().expect("the record reloads");
    });
}

/// An OFF session that named `worktree` with a fresh naming bind, then went
/// through a reload. Engram answers each naming bind with `replies`.
fn named_then_reloaded(
    label: &str,
    replies: Vec<ScriptedEngramControlResponse>,
) -> (ClaimedRoot, PathBuf, EngramSourceRootResponse) {
    let claimed = ClaimedRoot::new_scripted(label, replies);
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_tokenless_base_naming(&claimed, label);
    let named = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect("a fresh OFF session names its root with a naming bind");
    reload_naming_session(&claimed);
    (claimed, worktree, named)
}

fn claim_binding(label: &str) -> EngramControlWorkBinding {
    claimed_root_held(label).items[0]
        .control_binding
        .clone()
        .expect("the held claim carries its binding")
}

fn bind_key(bind: &RecordedEngramControlRequest) -> String {
    bind.request["idempotency_key"]
        .as_str()
        .expect("a session_bind carries its idempotency key")
        .to_owned()
}

/// A session_bind request without its idempotency key: what it asks for.
fn bind_intent(bind: &RecordedEngramControlRequest) -> Value {
    let mut request = bind.request.clone();
    request
        .as_object_mut()
        .expect("a session_bind is an object")
        .remove("idempotency_key");
    request
}

/// Puts `token`, flagged for rebinding, in place of the session's own: what
/// another binder's bind followed by a refusal or a reload leaves behind.
fn replace_flagged_token(claimed: &ClaimedRoot, token: &str) {
    claimed.record(|record| {
        record.engram.routing_token = Some(token.to_owned());
        record.engram.rebind_required = true;
    });
}

#[test]
fn a_reload_flags_the_naming_token_and_naming_again_rebinds_it_with_the_same_intent() {
    let label = "off-rebind-renamed";
    // The re-bind asks under its own key, so Engram answers it with a fresh
    // binding rather than replaying its answer to the first naming.
    let (claimed, worktree, first) = named_then_reloaded(
        label,
        vec![bind_reply("naming-token"), bind_reply("rebound-token")],
    );
    // The persisted form keeps the token, flagged, but not the work binding.
    assert_eq!(
        binding_state(&claimed),
        (Some("naming-token".to_owned()), true, None),
        "a reload restores the naming token flagged for rebinding"
    );

    let again = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect("the same root is named again under the same live claim");
    assert_eq!(again.root, first.root);
    assert_eq!(
        again.generation, first.generation,
        "the same name keeps its generation"
    );

    let binds = sent_binds(&claimed);
    assert_eq!(
        binds.len(),
        2,
        "naming again sends exactly one more session_bind"
    );
    assert_eq!(
        bind_intent(&binds[1]),
        bind_intent(&binds[0]),
        "the same naming intent"
    );
    assert_ne!(
        bind_key(&binds[1]),
        bind_key(&binds[0]),
        "under a key of its own, which Engram cannot answer with the first naming's token"
    );
    assert_eq!(binds[1].connection, binds[0].connection);
    assert_eq!(
        binding_state(&claimed),
        (
            Some("rebound-token".to_owned()),
            false,
            Some(claim_binding(label))
        ),
        "the answer is adopted and the flag cleared"
    );
    // What was saved is what a later reload restores.
    reload_naming_session(&claimed);
    assert_eq!(binding_state(&claimed).0.as_deref(), Some("rebound-token"));
    assert!(claimed.transport.requests().iter().all(|request| !matches!(
        request.request["operation"].as_str(),
        Some("turn_evaluate" | "turn_begin" | "turn_checkpoint")
    )));
}

#[test]
fn a_valid_unflagged_naming_token_is_used_without_another_bind() {
    let label = "off-rebind-unflagged";
    let claimed = ClaimedRoot::new_scripted(label, vec![bind_reply("naming-token")]);
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_tokenless_base_naming(&claimed, label);
    name_tokenless_base_root(&claimed, label, Some(&worktree)).expect("the first name binds");
    let before = binding_state(&claimed);
    assert!(!before.1);

    name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect("naming again with a valid token");
    assert_eq!(sent_binds(&claimed).len(), 1, "no second session_bind");
    assert_eq!(binding_state(&claimed), before);
}

#[test]
fn a_control_on_session_is_never_rebound_by_naming() {
    let label = "off-rebind-control-on";
    let (claimed, worktree, _) = named_then_reloaded(label, vec![bind_reply("naming-token")]);
    change_engram_settings(&claimed.state, |engram| engram.turn_gated_control = true);
    let before = binding_state(&claimed);
    assert!(before.1);

    // Whatever naming answers under control ON, it sends no naming bind.
    let _ = name_tokenless_base_root(&claimed, label, Some(&worktree));
    assert_eq!(
        sent_binds(&claimed).len(),
        1,
        "no naming bind under control ON"
    );
    assert_eq!(
        binding_state(&claimed),
        before,
        "the flag and token are its binders'"
    );
}

#[test]
fn naming_does_not_rebind_a_flagged_session_over_unresolved_state() {
    for case in [
        "active",
        "uncertain",
        "legacy",
        "checkpoint",
        "binding",
        "retained-bind",
    ] {
        let label = format!("off-rebind-fence-{case}");
        let (claimed, worktree, _) = named_then_reloaded(&label, vec![bind_reply("naming-token")]);
        let retained = {
            let inner = claimed.state.inner.lock().unwrap();
            AppState::engram_source_root_target_locked(&inner, &claimed.session_id)
                .unwrap()
                .map(|target| {
                    EngramRetainedBind::new(
                        &target,
                        &EngramControlRequest::SessionStatus {
                            routing_token: "naming-token".to_owned(),
                        },
                    )
                })
                .expect("the session has a Base target")
        };
        claimed.record(|record| match case {
            "active" => record.engram.active_grant_id = Some("keep-active".to_owned()),
            "uncertain" => record.engram.uncertain_grant_id = Some("keep-uncertain".to_owned()),
            "legacy" => record.engram.begins_recorded = false,
            "checkpoint" => record.engram.checkpoint_in_progress = true,
            "binding" => record.engram.bind_in_progress = true,
            "retained-bind" => record.engram.retained_bind = Some(retained),
            _ => unreachable!(),
        });
        let before = binding_state(&claimed);
        assert!(before.1, "{case}: flagged by the reload");

        let error = name_tokenless_base_root(&claimed, &label, Some(&worktree))
            .expect_err("naming refuses to rebind over unresolved state");
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}: {error:?}");
        assert!(
            error.message.contains("unresolved Engram")
                || error.message.contains("earlier Engram bind"),
            "{case}: the existing conflict: {error:?}"
        );
        assert_eq!(
            sent_binds(&claimed).len(),
            1,
            "{case}: no second session_bind"
        );
        assert_eq!(
            binding_state(&claimed),
            before,
            "{case}: flag and token kept"
        );
    }
}

#[test]
fn a_failed_save_of_the_rebind_restores_the_flagged_token() {
    let label = "off-rebind-save-fails";
    let (claimed, worktree, _) = named_then_reloaded(
        label,
        vec![bind_reply("naming-token"), bind_reply("naming-token")],
    );
    let before = binding_state(&claimed);
    assert!(before.1);

    TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(|fails| fails.set(true));
    let error = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect_err("a failed save refuses the name");
    TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(|fails| fails.set(false));
    assert!(
        error.message.contains("persisting naming binding"),
        "{error:?}"
    );
    assert_eq!(sent_binds(&claimed).len(), 2, "the bind itself was sent");
    assert_eq!(
        binding_state(&claimed),
        before,
        "the flagged token, prior work binding and flag are restored"
    );
    claimed.record(|record| {
        assert!(!record.engram.bind_in_progress);
        assert!(record.engram.source_root_bind_owner.is_none());
    });
}

#[test]
fn a_concurrent_replacement_of_the_binding_is_not_overwritten() {
    let label = "off-rebind-replaced";
    let (claimed, worktree, _) = named_then_reloaded(
        label,
        vec![bind_reply("naming-token"), bind_reply("naming-token")],
    );
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            // Another owner of the session's binding answers first.
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let engram = &mut inner.sessions[index].engram;
            engram.routing_token = Some("other-owner-token".to_owned());
            engram.rebind_required = false;
        }));
    });

    let error = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect_err("the naming bind no longer owns the binding");
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| hook.borrow_mut().take());
    assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
    assert_eq!(sent_binds(&claimed).len(), 2);
    let (token, flagged, _) = binding_state(&claimed);
    assert_eq!(
        token.as_deref(),
        Some("other-owner-token"),
        "the replacement stands"
    );
    assert!(!flagged);
}

#[test]
fn a_rebind_after_another_binder_replaced_the_naming_token_never_reuses_the_first_naming_key() {
    let label = "off-rebind-superseded";
    let (claimed, worktree, first) = named_then_reloaded(
        label,
        vec![
            bind_reply("naming-token"),
            bind_reply("fresh-token"),
            bind_reply("fresh-token"),
        ],
    );
    // Engram answers a key it has seen with the answer it recorded, so the
    // first naming's key would bring back "naming-token". Since then another
    // binder replaced it, and that binder's token is now flagged.
    replace_flagged_token(&claimed, "owed-bind-token");
    let before = binding_state(&claimed);

    // The re-bind's reply is lost to a failed save: the flagged state is
    // restored, as a lost reply leaves it.
    TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(|fails| fails.set(true));
    let error = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect_err("a failed save refuses the name");
    TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(|fails| fails.set(false));
    assert!(
        error.message.contains("persisting naming binding"),
        "{error:?}"
    );
    assert_eq!(binding_state(&claimed), before);

    let again = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect("naming again re-binds the flagged session");
    assert_eq!(again.generation, first.generation);

    let binds = sent_binds(&claimed);
    assert_eq!(binds.len(), 3);
    assert_eq!(bind_intent(&binds[1]), bind_intent(&binds[0]));
    assert_ne!(
        bind_key(&binds[1]),
        bind_key(&binds[0]),
        "the re-bind never asks under the key whose recorded answer is the superseded token"
    );
    assert_eq!(
        bind_key(&binds[2]),
        bind_key(&binds[1]),
        "while the flagged token is the same, a retried re-bind replays under the same key"
    );
    assert_eq!(
        binding_state(&claimed),
        (
            Some("fresh-token".to_owned()),
            false,
            Some(claim_binding(label))
        ),
        "the fresh binding is adopted and the flag cleared"
    );
}

fn with_named_roots<T>(
    claimed: &ClaimedRoot,
    change: impl FnOnce(&mut TestEngramNamedRoots) -> T,
) -> T {
    change(
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .expect("the named-roots model"),
    )
}

fn sent_named_root_binds(claimed: &ClaimedRoot) -> usize {
    claimed
        .transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .count()
}

fn nothing_named_yet(claimed: &ClaimedRoot) -> bool {
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_work_source_roots
        .is_empty()
}

/// Makes Engram's control session for the claimed session inactive right
/// after its next naming bind is answered, as an exited control session is.
fn control_session_goes_inactive_after_the_bind(claimed: &ClaimedRoot) {
    let transport = claimed.transport.clone();
    let session = claimed.session_id.clone();
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            transport
                .named_roots
                .lock()
                .unwrap()
                .as_mut()
                .expect("the named-roots model")
                .inactive_sessions
                .insert(session);
        }));
    });
}

#[test]
fn an_off_named_root_refusal_flags_the_held_token_and_naming_again_binds_the_root() {
    let label = "off-refused-rebind";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![bind_reply("naming-token"), bind_reply("rebound-token")],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_tokenless_base_naming(&claimed, label);
    control_session_goes_inactive_after_the_bind(&claimed);

    let error = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect_err("Engram refuses the named-root bind of an inactive control session");
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| hook.borrow_mut().take());
    assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
    assert!(
        error.message.contains("named_root_binding_refused")
            && error.message.contains("requires an active host session"),
        "{error:?}"
    );
    assert!(
        error.message.contains("marked for renewal")
            && error.message.contains("name the root again"),
        "the refusal names its remedy: {error:?}"
    );
    let (token, flagged, _) = binding_state(&claimed);
    assert_eq!(token.as_deref(), Some("naming-token"), "the token is kept");
    assert!(flagged, "the refused token is flagged for rebinding");
    assert!(nothing_named_yet(&claimed));

    // Naming the same root again re-binds the session, which is what makes
    // Engram's control session active again, and binds the root.
    let named = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect("naming again re-binds the session and binds the root");
    let binds = sent_binds(&claimed);
    assert_eq!(binds.len(), 2, "exactly one re-bind");
    assert_eq!(bind_intent(&binds[1]), bind_intent(&binds[0]));
    assert_ne!(
        bind_key(&binds[1]),
        bind_key(&binds[0]),
        "under a key Engram has not seen, so it does not replay the inactive binding"
    );
    assert!(
        with_named_roots(&claimed, |roots| roots.inactive_sessions.is_empty()),
        "the re-bind made Engram's control session active"
    );
    assert_eq!(
        binding_state(&claimed),
        (
            Some("rebound-token".to_owned()),
            false,
            Some(claim_binding(label))
        ),
        "the new token is adopted and the flag cleared"
    );
    assert_eq!(sent_named_root_binds(&claimed), 2);
    assert!(!nothing_named_yet(&claimed), "the root is bound: {named:?}");
    assert!(claimed.transport.requests().iter().all(|request| !matches!(
        request.request["operation"].as_str(),
        Some("turn_evaluate" | "turn_begin" | "turn_checkpoint")
    )));
}

#[test]
fn an_off_named_root_refusal_for_another_cause_flags_nothing() {
    let label = "off-refused-other-cause";
    let claimed = ClaimedRoot::new_scripted(label, vec![bind_reply("naming-token")]);
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_tokenless_base_naming(&claimed, label);
    // A refusal Engram makes for another reason than an inactive session.
    with_named_roots(&claimed, |roots| roots.refuse_next = true);

    let error = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect_err("Engram refuses the named-root bind");
    assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
    assert!(
        error.message.contains("named_root_binding_refused")
            && error.message.contains("does not cure this refusal"),
        "the refusal keeps Engram's reason and claims no inactive session: {error:?}"
    );
    assert!(!error.message.contains("no longer active"), "{error:?}");
    assert_eq!(
        binding_state(&claimed).0.as_deref(),
        Some("naming-token")
    );
    assert!(!binding_state(&claimed).1, "nothing is flagged");
    assert_eq!(sent_binds(&claimed).len(), 1, "no re-bind is owed");
}

#[test]
fn an_off_named_root_refusal_after_the_token_changed_flags_nothing() {
    let label = "off-refused-token-changed";
    let claimed = ClaimedRoot::new_scripted(label, vec![bind_reply("naming-token")]);
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_tokenless_base_naming(&claimed, label);
    control_session_goes_inactive_after_the_bind(&claimed);
    // Another binder replaces the session's token while the named-root bind
    // under the old one is in flight.
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    with_named_roots(&claimed, |roots| {
        roots.on_inactive_refusal = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.routing_token = Some("other-owner-token".to_owned());
        }));
    });

    let error = name_tokenless_base_root(&claimed, label, Some(&worktree))
        .expect_err("Engram refuses the named-root bind");
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| hook.borrow_mut().take());
    // The refusal's authority read re-checks the session's current token
    // first and refuses a read for a token that is no longer the session's;
    // the refusal then names that precondition and its remedy, and flags
    // nothing.
    assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
    assert!(
        error.message.contains("named_root_binding_refused")
            && error
                .message
                .contains("replaced while this naming's bind was in flight")
            && error.message.contains("name the root again"),
        "the refusal names the failed precondition and its remedy: {error:?}"
    );
    assert!(
        error.message.contains("earlier authority connection"),
        "the authority read's own refusal is kept: {error:?}"
    );
    // The fence before the read refused it: no read was sent for the stale
    // token after the refused bind.
    let operations: Vec<String> = claimed
        .transport
        .requests()
        .iter()
        .map(|request| request.request["operation"].as_str().unwrap_or("").to_owned())
        .collect();
    let refused_bind = operations
        .iter()
        .rposition(|operation| operation == "named_root_bind")
        .expect("the refused named-root bind");
    assert!(
        !operations[refused_bind + 1..]
            .iter()
            .any(|operation| operation == "named_root_read"),
        "no authority read follows the refused bind: {operations:?}"
    );
    let (token, flagged, _) = binding_state(&claimed);
    assert_eq!(token.as_deref(), Some("other-owner-token"), "the replacement stands");
    assert!(!flagged, "a token that is not the refused one is not flagged");
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal
            .iter()
            .any(|journal| journal.pending.is_some()),
        "the refused transition is neither learned nor cleared"
    );
}

#[test]
fn a_named_root_refusal_under_turn_gated_control_keeps_the_binding_and_names_its_remedy() {
    let label = "on-refused-remedy";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed.record(|record| record.engram.rebind_required = false);
    let before = binding_state(&claimed);
    assert!(!before.1, "an unflagged binding to start from");
    with_named_roots(&claimed, |roots| {
        roots.inactive_sessions.insert(claimed.session_id.clone());
    });

    let error = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .expect_err("Engram refuses the named-root bind");
    assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
    assert!(
        error.message.contains("named_root_binding_refused")
            && error.message.contains("Under turn-gated control")
            && error.message.contains("name the root again"),
        "the refusal names the failed precondition and its remedy: {error:?}"
    );
    assert_eq!(
        binding_state(&claimed),
        before,
        "the session's binder owns its binding; nothing is flagged"
    );
    assert!(sent_binds(&claimed).is_empty(), "naming binds nothing");
}
