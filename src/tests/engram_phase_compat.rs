//! Engram phase compatibility (tm-1s9u): the phases in which a session holds
//! no turn. Engram builds before the grant-page removal (Engram
//! w-106de4b03b57) report `sync_required` after a bind or a retired grant;
//! later builds report `ready` for the same state. TermAl accepts both where
//! it once required `sync_required`: the rebind check, and the proof that an
//! issued grant was retired, which decides whether a begin refused with
//! `grant_scope_mismatch` is evaluated once more.
//!
//! Owns the two predicates' truth tables and the rebind and re-evaluation
//! paths through them. Does not own other begin refusals
//! (`begin_refusal_expiry_policy_matches_the_external_engram_contract` in
//! `engram_host_adapter.rs`). New module beside the adapter tests, created
//! instead of growing them.

use super::*;

fn begin_refusal_reply(code: &str) -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Ok(json!({ "decision": "refuse", "code": code })))
}

/// A project with Engram control enabled and a parent session in it.
fn engram_project(label: &str) -> (AppState, mpsc::Receiver<CodexRuntimeCommand>, String) {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime(label);
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(format!("{label}-project"));
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram phase compatibility");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    (state, runtime_rx, parent_session_id)
}

fn create_reviewer(state: &AppState, parent_session_id: &str, prompt: &str) -> String {
    state
        .create_read_only_delegation(
            parent_session_id,
            CreateDelegationRequest {
                prompt: prompt.to_owned(),
                title: Some("Engram phase compatibility".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should start")
        .delegation
        .child_session_id
}

fn operations(transport: &ScriptedEngramControlTransport, session_id: &str) -> Vec<String> {
    transport
        .requests()
        .into_iter()
        .filter(|request| request.connection.session_id == session_id)
        .map(|request| {
            request.request["operation"]
                .as_str()
                .expect("operation should serialize")
                .to_owned()
        })
        .collect()
}

#[test]
fn engram_phases_that_hold_no_turn_cover_both_engram_builds() {
    for phase in ["ready", "sync_required"] {
        assert!(engram_phase_holds_no_turn(phase), "{phase}");
    }
    for phase in ["turn_open", "exited", "", "READY"] {
        assert!(!engram_phase_holds_no_turn(phase), "{phase}");
    }
    let status = |value: Value| {
        serde_json::from_value::<EngramSessionStatusResponse>(value).expect("status should decode")
    };
    // Builds after the grant-page removal report `ready` and no cursors.
    assert!(engram_status_proves_grant_retired(&status(
        json!({ "phase": "ready" })
    )));
    assert!(engram_status_proves_grant_retired(&status(
        json!({ "phase": "sync_required", "confirmed_cursor": 3 })
    )));
    for open in [
        json!({ "phase": "turn_open", "open_grant_id": "grant" }),
        json!({ "phase": "ready", "open_grant_id": "grant" }),
        json!({ "phase": "exited" }),
    ] {
        assert!(
            !engram_status_proves_grant_retired(&status(open.clone())),
            "{open}"
        );
    }
}

#[test]
fn a_rebind_accepts_ready_and_sync_required_but_not_an_open_turn() {
    for (phase, accepted) in [
        ("sync_required", true),
        ("ready", true),
        ("turn_open", false),
    ] {
        let label = format!("engram-rebind-phase-{phase}");
        let (state, _runtime_rx, parent_session_id) = engram_project(&label);
        state.install_control_test_transport(ScriptedEngramControlTransport::new([
            bind_reply("parent-token"),
            bind_reply("child-token"),
            grant_reply("first-grant"),
            begin_reply("first-grant"),
        ]));
        let child_id = create_reviewer(&state, &parent_session_id, "Bind, then rebind.");
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&child_id)
                .expect("child should exist");
            inner
                .session_mut_by_index(index)
                .expect("child should be mutable")
                .engram
                .rebind_required = true;
        }
        let rebind_transport = ScriptedEngramControlTransport::new([
            status_reply("ready"),
            ScriptedEngramControlResponse::Reply(Ok(json!({
                "routing_token": "rebound",
                "status": { "phase": phase }
            }))),
        ]);
        state.install_control_test_transport(rebind_transport.clone());
        let result = state.ensure_engram_session_bound_off_lock(&child_id);
        let (routing_token, rebind_required) = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let child = inner
                .sessions
                .iter()
                .find(|record| record.session.id == child_id)
                .expect("child should remain");
            (
                child.engram.routing_token.clone(),
                child.engram.rebind_required,
            )
        };
        // Either way the rebind is one status read and one bind, nothing more.
        assert_eq!(
            operations(&rebind_transport, &child_id),
            ["session_status", "session_bind"],
            "{phase}"
        );
        if accepted {
            result
                .expect("a rebind with no turn held should succeed")
                .expect("child should remain in Engram scope");
            assert_eq!(routing_token.as_deref(), Some("rebound"), "{phase}");
            assert!(!rebind_required, "{phase}: the rebind is done");
        } else {
            let Err(error) = result else {
                panic!("a rebind reporting an open turn must be refused");
            };
            assert!(
                error
                    .to_string()
                    .contains("instead of `ready` or `sync_required`"),
                "{error}"
            );
            assert_ne!(routing_token.as_deref(), Some("rebound"));
            assert!(rebind_required, "a refused rebind leaves the rebind armed");
        }
    }
}

#[test]
fn a_scope_mismatched_begin_is_evaluated_again_once_status_proves_the_grant_retired() {
    // Engram answers a begin for a grant it retired with grant_scope_mismatch.
    // A status with no open grant in a phase holding no turn proves the
    // retirement, so the prompt is evaluated once more instead of refused.
    for phase in ["sync_required", "ready"] {
        let label = format!("engram-scope-retired-{phase}");
        let (state, runtime_rx, parent_session_id) = engram_project(&label);
        let transport = ScriptedEngramControlTransport::new([
            bind_reply("parent-token"),
            bind_reply("child-token"),
            grant_reply("retired-grant"),
            begin_refusal_reply("grant_scope_mismatch"),
            status_reply(phase),
            grant_reply("fresh-grant"),
            begin_reply("fresh-grant"),
        ]);
        state.install_control_test_transport(transport.clone());
        let child_id = create_reviewer(&state, &parent_session_id, "Retry a retired grant.");
        assert_eq!(
            operations(&transport, &child_id),
            [
                "session_bind",
                "turn_evaluate",
                "turn_begin",
                "session_status",
                "turn_evaluate",
                "turn_begin",
            ],
            "{phase}"
        );
        let begins: Vec<Value> = transport
            .requests()
            .into_iter()
            .filter(|request| {
                request.connection.session_id == child_id
                    && request.request["operation"] == "turn_begin"
            })
            .map(|request| request.request["grant_id"].clone())
            .collect();
        assert_eq!(
            begins,
            [json!("retired-grant"), json!("fresh-grant")],
            "{phase}"
        );
        receive_synchronous_engram_prompt(&state, &runtime_rx, &label)
            .expect("the re-evaluated turn is delivered");
    }
}

#[test]
fn a_scope_mismatched_begin_is_not_retried_while_a_grant_is_still_open() {
    // `ready` alone is not proof: with a grant still open, the refusal stands
    // and no second grant is requested.
    let label = "engram-scope-open-grant";
    let (state, runtime_rx, parent_session_id) = engram_project(label);
    let transport = ScriptedEngramControlTransport::new([
        bind_reply("parent-token"),
        bind_reply("child-token"),
        grant_reply("open-grant"),
        begin_refusal_reply("grant_scope_mismatch"),
        ScriptedEngramControlResponse::Reply(Ok(json!({
            "phase": "ready",
            "open_grant_id": "open-grant"
        }))),
    ]);
    state.install_control_test_transport(transport.clone());
    let child_id = create_reviewer(&state, &parent_session_id, "Keep the open grant.");
    assert_eq!(
        operations(&transport, &child_id),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "session_status"
        ]
    );
    assert!(
        runtime_rx.try_recv().is_err(),
        "the refused begin is withheld, not delivered"
    );
}
