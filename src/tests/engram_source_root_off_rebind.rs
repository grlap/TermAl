// Owns the tests of source-root naming re-establishing a turn-gated-control
// OFF session's Base binding after a host reload flagged its restored token
// for rebinding: the naming bind it sends and the key it sends it under, what
// it adopts, the fences that refuse it, the rollback of a failed save, a
// concurrent replacement, and a token another binder replaced meanwhile.
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
