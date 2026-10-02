// Request-path tests for bounded acceptance evidence visibility. Owns the
// known omitted locator inventory, unread continuation boundaries and reasons,
// and the rule that not-shown evidence is not proof of absence. Reuses the
// parent module's deterministic reader fixtures; no tracker process is run.
use super::*;

#[test]
fn acceptance_omissions_metadata_shrinks_before_a_fitting_contract_is_refused() {
    for mode in ["independent_session", "same_session"] {
        for with_continuation in [false, true] {
            let (state, project, parent, root) = fixture();
            install_store(&state, &project, &root);
            let criterion = "c".repeat(if with_continuation { 60_000 } else { 62_000 });
            let mut full = full_receipt();
            full["work"]["acceptance"] = json!([criterion, "second criterion stays whole"]);
            let bare = parse_acceptance_evaluation_task(show_receipt(None), full.clone()).unwrap();
            if mode == "same_session" {
                build_same_session_acceptance_brief(&bare, None, MAX_ACCEPTANCE_BRIEF_BYTES)
                    .expect("the complete contract fits without the omission inventory");
            } else {
                build_acceptance_evaluator_brief(&bare, "/repo", MAX_ACCEPTANCE_BRIEF_BYTES)
                    .expect("the complete contract fits without the omission inventory");
            }
            let token = format!("captured-cursor:{}", "x".repeat(8_000));
            let mut show = show_receipt(None);
            show["notes"] = json!(
                (0..110)
                    .map(|index| {
                        let mut note = evidence_note(index, "older proof");
                        note["locator"] = json!(format!("{index:064x}"));
                        note
                    })
                    .collect::<Vec<_>>()
            );
            if with_continuation {
                show["notes_omitted"] = json!(7);
                show["notes_window"] = json!({"older": 7, "after": token,
                    "read_cut": {"project_position": 42}});
            }
            let reader = move |_: &EngramConnectionConfig, args: &[String], _: Duration| {
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(evidence_selection::canonical_core_receipt())
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&[mode])))
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items": [], "omitted": 0}))
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else {
                    Ok(show.clone())
                }
            };
            let wire = serde_json::to_value(
                state
                    .request_acceptance_evaluation_with_runner(
                        &parent,
                        evaluation_request(Some(Agent::Codex)),
                        reader,
                    )
                    .unwrap_or_else(|error| {
                        panic!(
                            "{mode}, continuation={with_continuation}: {}",
                            error.message
                        )
                    }),
            )
            .unwrap();
            let prompt = if mode == "same_session" {
                wire["brief"].as_str().unwrap()
            } else {
                wire["delegation"]["prompt"].as_str().unwrap()
            };
            assert!(prompt.len() <= MAX_ACCEPTANCE_BRIEF_BYTES);
            assert!(prompt.contains(&criterion));
            assert!(prompt.contains("second criterion stays whole"));
            assert!(prompt.contains("not shown to fit the brief"));
            assert!(
                !prompt.contains(&"x".repeat(100)),
                "no partial continuation is usable"
            );
            let omitted = &wire["evidenceOmissions"];
            let left_out = &omitted["leftOut"];
            assert!(left_out["count"].as_u64().unwrap() >= 70);
            assert_eq!(left_out["locators"].as_array().unwrap().len(), 64);
            assert_eq!(
                left_out["locators"][63],
                format!("{:064x}", left_out["count"].as_u64().unwrap() - 1)
            );
            if with_continuation {
                assert_eq!(omitted["unread"]["continuation"], token);
                assert_eq!(omitted["unread"]["continuationOmitted"], false);
                assert_eq!(omitted["unread"]["readCut"]["project_position"], 42);
                assert_eq!(omitted["unread"]["reason"], "entry_limit");
            }
            assert_eq!(
                compact_acceptance_evaluation_request_result(&wire)["evidenceOmissions"],
                *omitted
            );
        }
    }
}

// Both slots of the brief are bounded: known omitted records keep their
// identities, while unvisited pages are reported without invented note ids.
#[test]
fn acceptance_omissions_entry_limit_exposes_older_only_proof_to_the_requester() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let mut show = show_receipt(None);
    show["notes"] = json!(
        (0..110)
            .map(|index| evidence_note(
                index,
                if index == 0 {
                    "ONLY PROOF of criterion 1"
                } else {
                    "unrelated newer note"
                }
            ))
            .collect::<Vec<_>>()
    );
    show["notes_omitted"] = json!(7);
    show["notes_window"] = json!({"after": "older-cut-token", "older": 7,
        "read_cut": {"project_position": 42, "observed_at": "2026-09-30T22:00:00Z"}});
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            fixture_reader(
                Arc::default(),
                show,
                Ok(policy_receipt(Some(&["independent_session"]))),
            ),
        )
        .unwrap();
    let wire = serde_json::to_value(response).unwrap();
    let omitted = &wire["evidenceOmissions"];
    assert_eq!(omitted["leftOut"]["count"], 70);
    assert_eq!(omitted["leftOut"]["locatorsOmitted"], 6);
    assert_eq!(omitted["leftOut"]["locators"].as_array().unwrap().len(), 64);
    assert_eq!(omitted["leftOut"]["locators"][0], "00000006cafe");
    assert_eq!(omitted["unread"]["count"], 7);
    assert_eq!(omitted["unread"]["reason"], "entry_limit");
    assert_eq!(omitted["unread"]["continuation"], "older-cut-token");
    let prompt = wire["delegation"]["prompt"].as_str().unwrap();
    assert!(!prompt.contains("ONLY PROOF"));
    assert!(prompt.contains("77 older entries not shown"));
    assert!(prompt.contains("and 6 older"));
    assert!(prompt.contains("does not establish that proof is absent on the item"));
    assert!(prompt.contains("say 'not shown' in the rationale"));
    assert_eq!(
        compact_acceptance_evaluation_request_result(&wire)["evidenceOmissions"],
        *omitted
    );
}

#[test]
fn acceptance_omissions_byte_limit_reports_known_locators_without_changing_verdicts() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let mut show = show_receipt(None);
    show["notes"] = json!(
        (0..3)
            .map(|index| evidence_note(
                index,
                &format!("{} criterion {} only proof", "x".repeat(40_000), index + 1)
            ))
            .collect::<Vec<_>>()
    );
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            fixture_reader(
                Arc::default(),
                show,
                Ok(policy_receipt(Some(&["independent_session"]))),
            ),
        )
        .unwrap();
    let wire = serde_json::to_value(response).unwrap();
    let clipped = &wire["evidenceOmissions"]["clipped"];
    assert!(clipped["count"].as_u64().unwrap() > 0);
    assert_eq!(clipped["locators"][0], "00000000cafe");
    assert_eq!(clipped["locatorsOmitted"], 0);
    let prompt = wire["delegation"]["prompt"].as_str().unwrap();
    assert!(prompt.len() <= MAX_ACCEPTANCE_BRIEF_BYTES);
    assert!(prompt.contains("say 'not shown' in the rationale"));
    assert!(prompt.contains("pass, fail, insufficient-evidence"));
    assert_eq!(acceptance_verdict_word("not shown"), None);
}

#[test]
fn acceptance_omissions_page_limit_and_time_budget_keep_the_last_read_cut() {
    for timed in [false, true] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let calls = Arc::new(AtomicUsize::new(0));
        let start = std::time::Instant::now();
        let now = || {
            start
                + if timed && calls.load(Ordering::SeqCst) >= 4 {
                    Duration::from_secs(30)
                } else {
                    Duration::ZERO
                }
        };
        let deadline = start + acceptance_evaluation_paging_reserve() + Duration::from_secs(25);
        let response = state
            .request_acceptance_evaluation_until(
                &parent,
                evaluation_request(None),
                endless_evidence_reader(calls.clone()),
                deadline,
                now,
            )
            .unwrap();
        let wire = serde_json::to_value(response).unwrap();
        let unread = &wire["evidenceOmissions"]["unread"];
        assert_eq!(unread["count"], 99);
        assert_eq!(unread["locatorsKnown"], false);
        assert_eq!(
            unread["reason"],
            if timed { "time_budget" } else { "page_limit" }
        );
        assert_eq!(
            unread["continuation"],
            if timed { "token-3" } else { "token-8" }
        );
        assert_eq!(calls.load(Ordering::SeqCst), if timed { 9 } else { 14 });
    }
}

#[test]
fn acceptance_omissions_failed_second_page_preserves_the_unread_locator_boundary() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let mut first = show_receipt(None);
    first["notes_omitted"] = json!(5);
    first["notes_window"] = json!({"after": "first-token", "older": 5,
        "read_cut": {"project_position": 42, "observed_at": "2026-09-30T22:00:00Z"}});
    let pages = Arc::new(AtomicUsize::new(0));
    let reads = pages.clone();
    let reader = move |_: &EngramConnectionConfig, args: &[String], _: Duration| {
        if args.iter().any(|arg| arg == "inspect") {
            Ok(evidence_selection::canonical_core_receipt())
        } else if args.first().map(String::as_str) == Some("control-policy") {
            Ok(policy_receipt(Some(&["independent_session"])))
        } else if args.iter().any(|arg| arg == "held") {
            Ok(json!({"items": [], "omitted": 0}))
        } else if args.iter().any(|arg| arg == "--full") {
            Ok(full_receipt())
        } else if args.iter().any(|arg| arg == "--after") {
            if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(json!({"notes": [evidence_note(1, "scattered older proof")],
                    "notes_window": {"after": "second-token", "older": 4,
                        "read_cut": {"project_position": 42, "observed_at": "2026-09-30T22:00:00Z"}}}))
            } else {
                Err(EngramTransportError::transport("cut expired"))
            }
        } else {
            Ok(first.clone())
        }
    };
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            reader,
        )
        .err()
        .expect("an issued page failure refuses the canonical source capture");
    assert_eq!(response.status, StatusCode::BAD_GATEWAY);
    assert!(response.message.contains("cut expired"));
    assert_eq!(pages.load(Ordering::SeqCst), 2);
    assert!(state.inner.lock().unwrap().delegations.is_empty());
}

#[test]
fn acceptance_omissions_missing_or_oversized_continuation_is_explicit() {
    for oversized in [false, true] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let mut show = show_receipt(None);
        show["notes"] = json!(
            (0..40)
                .map(|i| evidence_note(i, "newer context"))
                .collect::<Vec<_>>()
        );
        show["notes_omitted"] = json!(1);
        show["notes_window"] = json!({"older": 1, "after": if oversized {
            Some("x".repeat(MAX_ACCEPTANCE_CONTINUATION_BYTES + 1)) } else { None }});
        let wire = serde_json::to_value(
            state
                .request_acceptance_evaluation_with_runner(
                    &parent,
                    evaluation_request(Some(Agent::Codex)),
                    fixture_reader(
                        Arc::default(),
                        show,
                        Ok(policy_receipt(Some(&["independent_session"]))),
                    ),
                )
                .unwrap(),
        )
        .unwrap();
        let unread = &wire["evidenceOmissions"]["unread"];
        assert_eq!(unread["count"], 1);
        assert!(unread["continuation"].is_null());
        assert_eq!(unread["continuationOmitted"], oversized);
        assert_eq!(
            unread["reason"],
            if oversized {
                "entry_limit"
            } else {
                "missing_continuation"
            }
        );
        assert_eq!(
            unread["continuationBytes"],
            if oversized {
                MAX_ACCEPTANCE_CONTINUATION_BYTES + 1
            } else {
                0
            }
        );
        let prompt = wire["delegation"]["prompt"].as_str().unwrap();
        assert!(prompt.contains("individual unread locators are unknown"));
        assert!(!prompt.contains(&"x".repeat(MAX_ACCEPTANCE_CONTINUATION_BYTES)));
    }
}

#[test]
fn acceptance_omissions_final_page_without_window_drops_the_consumed_boundary() {
    for remaining in [0, 1] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let mut first = show_receipt(None);
        first["notes_window"] = json!({"after": "consumed-token", "older": 5,
            "read_cut": {"project_position": 42}});
        first["notes_omitted"] = json!(5);
        let reader = move |_: &EngramConnectionConfig, args: &[String], _: Duration| {
            if args.iter().any(|arg| arg == "inspect") {
                Ok(evidence_selection::canonical_core_receipt())
            } else if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&["independent_session"])))
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items": [], "omitted": 0}))
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full_receipt())
            } else if args.iter().any(|arg| arg == "--after") {
                Ok(
                    json!({"notes": [evidence_note(3, "final older proof")], "notes_omitted": remaining}),
                )
            } else {
                Ok(first.clone())
            }
        };
        let wire = serde_json::to_value(
            state
                .request_acceptance_evaluation_with_runner(
                    &parent,
                    evaluation_request(Some(Agent::Codex)),
                    reader,
                )
                .unwrap(),
        )
        .unwrap();
        let unread = &wire["evidenceOmissions"]["unread"];
        assert_eq!(unread["count"], remaining);
        assert!(unread["continuation"].is_null());
        assert!(unread["readCut"].is_null());
        assert_eq!(
            unread["reason"],
            if remaining == 0 {
                Value::Null
            } else {
                json!("missing_continuation")
            }
        );
        let prompt = wire["delegation"]["prompt"].as_str().unwrap();
        assert!(prompt.contains("final older proof"));
        assert!(!prompt.contains("consumed-token"));
    }
}
