// Owns Kimi tracker-read admission regressions, using the ACP fixtures from
// kimi_read_only.rs. Does not exercise tracker execution or change its policy.

use super::*;

#[test]
fn tracker_reads_with_complete_arguments_and_matching_summary_are_allowed() {
    let calls = [
        ("next", json!({ "peek": true, "limit": 20 })),
        ("ls", json!({ "ready": true, "limit": 20 })),
        ("search", json!({ "query": "review" })),
        (
            "show",
            json!({ "work_ref": "example", "notes": true, "gates": true }),
        ),
        ("memories", json!({})),
    ];
    let mut refused = Vec::new();
    for (word, args) in calls {
        let title = format!("mcp__engram__{word}");
        let decision = decide(
            &mcp_params(&title),
            Some(&observed(&title, &args.to_string())),
        );
        if decision != (KimiReadOnlyDecision::Allow { args }) {
            refused.push((word, decision));
        }
    }
    assert!(
        refused.is_empty(),
        "shared tracker reads were refused: {refused:?}"
    );
}

fn install_tracker(kimi: &KimiHarness) {
    let mut inner = kimi.state.inner.lock().unwrap();
    let index = inner.find_session_index(&kimi.id).unwrap();
    inner.sessions[index].engram_mcp_installed = Some(EngramMcpInstalledDescriptor {
        binary_path: "C:/tools/engram.exe".to_owned(),
        home: "C:/engram-home".to_owned(),
        actor_id: "dev/reviewer".to_owned(),
        actor_context: None,
        store_key: None,
    });
    assert!(read_only_child_tracker_reads_allowed_locked(
        &inner, &kimi.id
    ));
}

#[test]
fn tracker_read_is_approved_once_in_the_observed_acp_order() {
    let mut kimi = KimiHarness::read_only_child();
    install_tracker(&kimi);
    let title = "mcp__engram__memories";
    kimi.request(
        "tracker",
        title,
        "{}",
        &format!("{KIMI_MCP_SUMMARY_PREFIX}{title}"),
    );
    assert_eq!(
        next_permission_outcome(&kimi.rx),
        json!({ "outcome": "selected", "optionId": "approve_once" })
    );
    kimi.send(update(raw_input("tracker", json!({}))));
    assert!(
        kimi.rx.try_recv().is_err(),
        "matching input must keep the turn running"
    );
}

#[test]
fn tracker_writes_advancing_reads_and_unknown_arguments_remain_refused() {
    for word in [
        "add",
        "claim",
        "update",
        "gate",
        "evaluate",
        "note",
        "done",
        "handoff",
        "remember",
        "forget",
        "task_sync_settings",
    ] {
        let title = format!("mcp__engram__{word}");
        assert!(
            is_reject(&decide(&mcp_params(&title), Some(&observed(&title, "{}")))),
            "{word}"
        );
    }
    for (word, args) in [
        ("next", json!({})),
        ("next", json!({ "peek": false })),
        ("next", json!({ "peek": "true" })),
        ("memories", json!({ "context_generation": "generation" })),
        ("show", json!({ "work_ref": "example", "unknown": true })),
    ] {
        let title = format!("mcp__engram__{word}");
        assert!(
            is_reject(&decide(
                &mcp_params(&title),
                Some(&observed(&title, &args.to_string()))
            )),
            "{word}: {args}"
        );
    }
    for (word, _) in ENGRAM_MCP_READ_TOOLS {
        let title = format!("mcp__engram__{word}");
        let args = if *word == "next" {
            json!({ "peek": true, "unknown": true })
        } else {
            json!({ "unknown": true })
        };
        assert!(
            is_reject(&decide(
                &mcp_params(&title),
                Some(&observed(&title, &args.to_string()))
            )),
            "{word}: unknown arguments must fail closed"
        );
    }
}

#[test]
fn tracker_reads_require_host_admission_exact_identity_and_summary() {
    let title = "mcp__engram__memories";
    let observation = observed(title, "{}");
    assert!(is_reject(&kimi_read_only_permission_decision(
        &mcp_params(title),
        Some(&observation),
        WORKDIR,
        false,
        &|_| true,
    )));
    for alias in [
        "memories",
        "mcp__engram-x__memories",
        "mcp__engram__",
        "mcp__engram__future_read",
    ] {
        assert!(
            is_reject(&decide(&mcp_params(alias), Some(&observed(alias, "{}")))),
            "{alias}"
        );
    }
    for summary in [
        "",
        title,
        "Requesting approval to Approve mcp__engram__show",
    ] {
        assert!(
            is_reject(&decide(
                &request_params("m", title, summary),
                Some(&observation)
            )),
            "{summary}"
        );
    }
    assert!(is_reject(&decide(
        &mcp_params(title),
        Some(&observed("mcp__engram__show", "{}"))
    )));
}

#[test]
fn tracker_reads_require_one_complete_bounded_argument_object() {
    let title = "mcp__engram__memories";
    let params = mcp_params(title);
    assert!(is_reject(&decide(&params, None)));
    for text in [
        "",
        "{",
        "null",
        "[]",
        "{} {}",
        r#"{"query":"a","query":"b"}"#,
    ] {
        assert!(
            is_reject(&decide(&params, Some(&observed(title, text)))),
            "{text}"
        );
    }
    let mut missing = observed(title, "{}");
    missing.streamed_args = None;
    assert!(is_reject(&decide(&params, Some(&missing))));
    let mut oversized = observed(title, "{}");
    oversized.oversized = true;
    assert!(is_reject(&decide(&params, Some(&oversized))));
}

#[test]
fn tracker_acp_admission_uses_the_existing_shared_host_predicate() {
    for case in ["missing-server", "wrong-parent", "evaluator", "not-running"] {
        let mut kimi = KimiHarness::read_only_child();
        install_tracker(&kimi);
        {
            let mut inner = kimi.state.inner.lock().unwrap();
            let index = inner.find_session_index(&kimi.id).unwrap();
            let delegation = inner
                .find_delegation_index_by_child_session_id(&kimi.id)
                .unwrap();
            match case {
                "missing-server" => inner.sessions[index].engram_mcp_installed = None,
                "wrong-parent" => {
                    inner.sessions[index].session.parent_delegation_id = Some("missing".to_owned())
                }
                "evaluator" => inner.delegations[delegation].mode = DelegationMode::Evaluator,
                "not-running" => inner.delegations[delegation].status = DelegationStatus::Queued,
                _ => unreachable!(),
            }
            assert!(
                !read_only_child_tracker_reads_allowed_locked(&inner, &kimi.id),
                "{case}"
            );
        }
        let title = "mcp__engram__memories";
        kimi.request(
            "tracker",
            title,
            "{}",
            &format!("{KIMI_MCP_SUMMARY_PREFIX}{title}"),
        );
        assert_eq!(
            next_permission_outcome(&kimi.rx),
            json!({ "outcome": "selected", "optionId": "reject" }),
            "{case}"
        );
    }
}

#[test]
fn tracker_acp_without_allow_once_is_refused() {
    let mut kimi = KimiHarness::read_only_child();
    install_tracker(&kimi);
    let title = "mcp__engram__memories";
    kimi.send(update(tool_call("tracker", title)));
    kimi.send(update(streamed("tracker", "{}")));
    let mut params = request_params(
        "tracker",
        title,
        &format!("{KIMI_MCP_SUMMARY_PREFIX}{title}"),
    );
    params["options"] = json!([
        { "optionId": "always", "kind": "allow_always" },
        { "optionId": "reject", "kind": "reject_once" }
    ]);
    kimi.send(permission_message(params));
    assert_eq!(
        next_permission_outcome(&kimi.rx),
        json!({ "outcome": "selected", "optionId": "reject" })
    );
}

#[test]
fn tracker_acp_retains_stop_and_runtime_fences() {
    for stopping in [false, true] {
        let kimi = KimiHarness::read_only_child();
        install_tracker(&kimi);
        let token = if stopping {
            let mut inner = kimi.state.inner.lock().unwrap();
            let index = inner.find_session_index(&kimi.id).unwrap();
            inner.sessions[index].runtime_stop_in_progress = true;
            "kimi-read-only-runtime"
        } else {
            "old-runtime"
        };
        let title = "mcp__engram__memories";
        let mut observations = KimiReadOnlyObservations::default();
        observations.observe(&tool_call("tracker", title));
        observations.observe(&streamed("tracker", "{}"));
        assert!(
            kimi.state
                .answer_kimi_read_only_permission(
                    &permission_message(request_params(
                        "tracker",
                        title,
                        &format!("{KIMI_MCP_SUMMARY_PREFIX}{title}")
                    )),
                    &kimi.id,
                    &RuntimeToken::Acp(token.to_owned()),
                    &kimi.runtime.input_tx,
                    &mut observations,
                )
                .unwrap()
        );
        assert_eq!(
            next_permission_outcome(&kimi.rx),
            json!({ "outcome": "cancelled" })
        );
        assert!(observations.get("tracker").unwrap().approved_args.is_none());
    }
}

#[test]
fn changed_tracker_arguments_after_approval_stop_the_turn() {
    let mut kimi = KimiHarness::read_only_child();
    install_tracker(&kimi);
    let title = "mcp__engram__memories";
    kimi.request(
        "tracker",
        title,
        "{}",
        &format!("{KIMI_MCP_SUMMARY_PREFIX}{title}"),
    );
    assert_eq!(
        next_permission_outcome(&kimi.rx),
        json!({ "outcome": "selected", "optionId": "approve_once" })
    );
    kimi.send(update(raw_input(
        "tracker",
        json!({ "context_generation": "generation" }),
    )));
    assert!(kimi.cancelled());
    assert_eq!(kimi.status(), SessionStatus::Error);
}
