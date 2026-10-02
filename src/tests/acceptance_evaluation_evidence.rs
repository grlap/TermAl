// Criterion evidence discovery through the production request and brief.
// Uses the parent fixtures; no live tracker is touched.
use super::*;

// Compact continuations are generated from the producer's compact header and
// window schema, rather than by cloning an initial show response.
fn compact_notes_chain() -> (Value, Value, Value, Value) {
    let (mut show, full, binding) = canonical_binding_receipts();
    show["notes_omitted"] = json!(1);
    show["notes_window"] = json!({
        "selection":"newest_first", "order":"oldest_first", "newer":0,
        "older":1, "shown":1, "total":2, "after":"captured-notes-page",
        "includes_gates":true, "read_cut":{"project_position":120,
            "observed_at":"2026-10-01T12:00:00Z", "valid_until_ms":1790000000000_i64}
    });
    let page = json!({"work":{"short_ref":"w-task", "title":"Task"},
        "full_detail":"engram work show w-task --full",
        "notes":[{"locator":"aaaaaaaa7777", "kind":"generic", "family":"notes",
            "summary":"Older criterion evidence"}], "notes_omitted":1,
        "notes_window":{"selection":"newest_first", "order":"oldest_first",
            "newer":1, "older":0, "shown":1, "total":2, "after":null,
            "includes_gates":true, "read_cut":{"project_position":120,
                "observed_at":"2026-10-01T12:00:01Z", "valid_until_ms":1790000001000_i64}}});
    (show, full, binding, page)
}

fn assert_nullable_catalog_expiry(single_page: bool, initial_null: bool, page_null: bool) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (mut show, full, binding, mut page) = compact_notes_chain();
    if initial_null {
        show["notes_window"]["read_cut"]["valid_until_ms"] = Value::Null;
    }
    if page_null {
        page["notes_window"]["read_cut"]["valid_until_ms"] = Value::Null;
    }
    if single_page {
        show["notes_omitted"] = json!(0);
        show["notes_window"]["older"] = json!(0);
        show["notes_window"]["total"] = json!(1);
        show["notes_window"]["after"] = Value::Null;
    }
    install_binding_evidence_transport(&state, &parent, vec![binding]);
    let pages = AtomicUsize::new(0);
    let core_reads = AtomicUsize::new(0);
    let result = state.request_acceptance_evaluation_with_runner(
        &parent,
        evaluation_request(None),
        |_, args, _| {
            if args.iter().any(|arg| arg == "inspect") {
                core_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--after") {
                pages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(page.clone())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items":[], "omitted":0}))
            } else if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&["same_session"])))
            } else {
                Ok(show.clone())
            }
        },
    );
    assert!(
        result.is_ok(),
        "nullable catalog expiry: {:?}",
        result.err()
    );
    assert_eq!(
        pages.load(std::sync::atomic::Ordering::SeqCst),
        usize::from(!single_page)
    );
    assert_eq!(core_reads.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn criterion_evidence_carrier_initial_catalog_expiry_can_be_null() {
    assert_nullable_catalog_expiry(true, true, false);
}

#[test]
fn criterion_evidence_carrier_compact_catalog_expiry_can_be_null() {
    assert_nullable_catalog_expiry(false, false, true);
    assert_nullable_catalog_expiry(false, true, true);
    assert_nullable_catalog_expiry(false, true, false);
}

fn assert_failed_cli_catalog_page_is_not_admitted(make_error: fn() -> EngramTransportError) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, binding, _) = compact_notes_chain();
    let transport = install_binding_evidence_transport(&state, &parent, vec![binding]);
    let pages = AtomicUsize::new(0);
    let result = state.request_acceptance_evaluation_with_runner(
        &parent,
        evaluation_request(None),
        |_, args, _| {
            if args.iter().any(|arg| arg == "inspect") {
                Ok(canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--after") {
                pages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(make_error())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items":[], "omitted":0}))
            } else if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&["same_session"])))
            } else {
                Ok(show.clone())
            }
        },
    );
    let error = result
        .err()
        .expect("a failed canonical CLI page must not admit an evaluation");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert!(
        error.message.contains("canonical discovery page"),
        "{error:?}"
    );
    assert_eq!(pages.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(transport.requests.lock().unwrap().is_empty());
    assert!(state.inner.lock().unwrap().delegations.is_empty());
}

#[test]
fn criterion_evidence_carrier_cli_cursor_refusal_is_fatal() {
    // Nonzero CLI exit is Transport at run_engram_json_command, even when
    // the store refused a stale/expired opaque cursor instead of changing A/E.
    assert_failed_cli_catalog_page_is_not_admitted(|| {
        EngramTransportError::transport(
            "Engram work show failed: catalog changed; start a fresh listing",
        )
    });
}

#[test]
fn criterion_evidence_carrier_cli_malformed_stdout_is_fatal() {
    // Successful CLI exit with malformed JSON is Protocol at the same reader.
    assert_failed_cli_catalog_page_is_not_admitted(|| {
        EngramTransportError::protocol("invalid Engram work show response: malformed stdout")
    });
}

#[test]
fn criterion_evidence_carrier_compact_pages_preserve_the_canonical_bracket() {
    for change in [
        "none",
        "ref",
        "project_cut",
        "work_id",
        "basis",
        "null_basis",
        "duplicate_ref",
        "missing_header",
        "bad_header",
        "count",
        "order",
        "cursor",
        "expiry_zero",
        "expiry_negative",
        "expiry_wrong_type",
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (mut show, full, binding, mut page) = compact_notes_chain();
        match change {
            "ref" => page["work"]["short_ref"] = json!("w-other"),
            "project_cut" => page["notes_window"]["read_cut"]["project_position"] = json!(121),
            "work_id" => page["work"]["work_id"] = json!("01a0b6c5-4b4f-7b41-9e32-edc597077aaa"),
            "basis" => page["evidence_basis"] = json!(43),
            "null_basis" => page["evidence_basis"] = Value::Null,
            "duplicate_ref" => page["status"] = json!({"work":{"short_ref":"w-other"}}),
            "missing_header" => {
                page.as_object_mut().unwrap().remove("work");
            }
            "bad_header" => page["work"]["short_ref"] = json!(17),
            "count" => page["notes_window"]["shown"] = json!(2),
            "order" => page["notes_window"]["order"] = json!("newest_first"),
            "expiry_zero" => page["notes_window"]["read_cut"]["valid_until_ms"] = json!(0),
            "expiry_negative" => page["notes_window"]["read_cut"]["valid_until_ms"] = json!(-1),
            "expiry_wrong_type" => {
                page["notes_window"]["read_cut"]["valid_until_ms"] = json!("tomorrow");
            }
            "cursor" => {
                show["notes_omitted"] = json!(2);
                show["notes_window"]["older"] = json!(2);
                show["notes_window"]["total"] = json!(3);
                page["notes_omitted"] = json!(2);
                page["notes_window"]["older"] = json!(1);
                page["notes_window"]["total"] = json!(3);
                page["notes_window"]["after"] = json!("captured-notes-page");
            }
            _ => {}
        }
        let transport = install_binding_evidence_transport(&state, &parent, vec![binding]);
        let calls = Mutex::new(Vec::new());
        let result = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |_, args, _| {
                calls.lock().unwrap().push(args.to_vec());
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--after") {
                    Ok(page.clone())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[], "omitted":0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["same_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        );
        if change == "none" {
            assert!(
                result.is_ok(),
                "faithful compact page must succeed: {:?}",
                result.err()
            );
            let calls = calls.lock().unwrap();
            assert_eq!(
                calls
                    .iter()
                    .filter(|args| args.iter().any(|arg| arg == "inspect"))
                    .count(),
                2
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|args| args.iter().any(|arg| arg == "--after"))
                    .count(),
                1
            );
            let closing = calls
                .iter()
                .rposition(|args| args.iter().any(|arg| arg == "inspect"))
                .unwrap();
            assert!(calls[closing - 1].iter().any(|arg| arg == "--notes"));
        } else {
            let error = result.err().expect(change);
            assert_eq!(
                error.status,
                match change {
                    "missing_header" | "bad_header" | "count" | "order" | "expiry_zero"
                    | "expiry_negative" | "expiry_wrong_type" => StatusCode::BAD_GATEWAY,
                    _ => StatusCode::CONFLICT,
                },
                "{change}: {error:?}"
            );
            assert!(
                error.message.contains("notes continuation"),
                "{change}: {error:?}"
            );
            assert!(transport.requests.lock().unwrap().is_empty(), "{change}");
            assert!(state.inner.lock().unwrap().delegations.is_empty());
        }
    }
}

#[test]
fn criterion_evidence_carrier_held_rows_match_the_canonical_identity() {
    for change in ["work_id", "ref"] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, full, binding) = canonical_binding_receipts();
        assert!(show["status"]["work"].get("work_id").is_none());
        let mut row = json!({"work_id":canonical_core_receipt()["status"]["work"]["work_id"],
            "short_ref":"w-task", "claim_id":"claim-task"});
        if change == "work_id" {
            row["work_id"] = json!("01a0b6c5-4b4f-7b41-9e32-edc597077aaa");
        } else {
            row["short_ref"] = json!("w-other");
        }
        install_binding_evidence_transport(&state, &parent, vec![binding]);
        let result = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |_, args, _| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[row], "omitted":0}))
                } else if args.iter().any(|arg| arg == "inspect") {
                    Ok(canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["same_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        );
        let error = result
            .err()
            .expect("a contradictory canonical claim must be refused");
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert!(
            error
                .message
                .contains("requested claim has inconsistent work identity"),
            "{change}: {error:?}"
        );
        assert!(state.inner.lock().unwrap().delegations.is_empty());
    }
}

#[test]
fn criterion_evidence_request_paged_window_cannot_cross_the_core_bracket() {
    for failure in ["cut", "timeout"] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, full, page, continuation) = compact_notes_chain();
        let transport = install_binding_evidence_transport(&state, &parent, vec![page]);
        let pages = AtomicUsize::new(0);
        let error = state
            .request_acceptance_evaluation_with_runner(
                &parent,
                evaluation_request(None),
                |_, args, _| {
                    if args.iter().any(|arg| arg == "inspect") {
                        Ok(canonical_core_receipt())
                    } else if args.iter().any(|arg| arg == "--after") {
                        pages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if failure == "timeout" {
                            return Err(EngramTransportError::deadline("captured page timed out"));
                        }
                        let mut moved = continuation.clone();
                        moved["evidence_basis"] = json!(43);
                        Ok(moved)
                    } else if args.iter().any(|arg| arg == "--notes") {
                        Ok(show.clone())
                    } else if args.iter().any(|arg| arg == "--full") {
                        Ok(full.clone())
                    } else {
                        panic!("no admission after failed canonical window discovery: {args:?}")
                    }
                },
            )
            .err()
            .unwrap();
        assert_eq!(
            error.status,
            if failure == "timeout" {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::CONFLICT
            }
        );
        assert!(error.message.contains(if failure == "timeout" {
            "timed out"
        } else {
            "basis changed"
        }));
        assert_eq!(pages.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(transport.requests.lock().unwrap().is_empty());
        assert!(state.inner.lock().unwrap().delegations.is_empty());
    }
}

#[test]
fn criterion_evidence_request_brackets_pinned_projection_identity() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, page) = canonical_binding_receipts();
    assert!(show["status"]["work"].get("work_id").is_none());
    assert!(full["work"].get("active_run_id").is_none());
    let transport = install_binding_evidence_transport(&state, &parent, vec![page]);
    let calls = Mutex::new(Vec::new());
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |_, args, _| {
                calls.lock().unwrap().push(args.to_vec());
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[], "omitted":0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["same_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        )
        .unwrap();
    let wire = serde_json::to_value(response).unwrap();
    assert!(
        wire["brief"]
            .as_str()
            .unwrap()
            .contains("original obligation closure")
    );
    let calls = calls.lock().unwrap();
    let core: Vec<_> = calls
        .iter()
        .enumerate()
        .filter(|(_, args)| args.iter().any(|arg| arg == "inspect"))
        .collect();
    assert_eq!(core.len(), 2);
    assert_eq!(core[0].0, 0);
    assert!(calls[core[1].0 - 1].iter().any(|arg| arg == "--notes"));
    assert!(calls.iter().all(|args| {
        !args
            .iter()
            .any(|arg| matches!(arg.as_str(), "bind" | "enable" | "focus"))
    }));
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}

#[test]
fn criterion_evidence_request_refuses_crossed_core_brackets() {
    for change in [
        "before_window",
        "during_body",
        "after_closure",
        "between_final_reads",
        "revision",
        "cut",
        "work",
        "run_association",
        "malformed",
        "authority",
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, full, mut page) = canonical_binding_receipts();
        let replacement = "01a0b6c5-4b4f-7b41-9e32-edd7dea8b370";
        if change == "before_window" {
            page["basis"]["run_id"] = json!(replacement);
        }
        install_binding_evidence_transport(&state, &parent, vec![page]);
        let cores = AtomicUsize::new(0);
        let exact = AtomicUsize::new(0);
        let request = serde_json::from_value(json!({"workRef":"w-task", "criterionEvidence":[
            {"criterion":1,"locators":["cccccccccccccccccccccccccccccccc"]}]}))
        .unwrap();
        let error = state.request_acceptance_evaluation_with_runner(&parent, request, |_, args, _| {
            if args.iter().any(|arg| arg == "inspect") {
                let closing = cores.fetch_add(1, std::sync::atomic::Ordering::SeqCst) != 0;
                let mut receipt = canonical_core_receipt();
                if closing && matches!(change, "during_body" | "after_closure" | "between_final_reads") {
                    receipt["status"]["work"]["active_run_id"] = json!(replacement);
                    receipt["run"]["run_id"] = json!(replacement);
                }
                match change {
                    "revision" if closing => receipt["status"]["work"]["revision"] = json!(8),
                    "work" if closing => receipt["status"]["work"]["work_id"] = json!(replacement),
                    "run_association" => receipt["run"]["work_id"] = json!(replacement),
                    "malformed" => receipt["status"]["work"]["active_run_id"] = json!(17),
                    "authority" if closing => {
                        let mut inner = state.inner.lock().unwrap();
                        let index = inner.find_session_index(&parent).unwrap();
                        inner.sessions[index].engram.routing_token = Some("replacement-token".to_owned());
                    }
                    _ => {}
                }
                Ok(receipt)
            } else if args.iter().any(|arg| arg == "--note") {
                exact.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(json!({"work_ref":"w-task", "note":{"locator":"cccccccccccccccccccccccccccccccc",
                    "family":"notes", "summary":"Captured proof before replacement"}}))
            } else if args.iter().any(|arg| arg == "--full") { Ok(full.clone()) }
            else if args.iter().any(|arg| arg == "held") { Ok(json!({"items":[], "omitted":0})) }
            else {
                assert!(!args.iter().any(|arg| arg == "control-policy"), "{change}: no evaluator admission after a crossed bracket");
                let mut receipt = show.clone();
                if change == "cut" && exact.load(std::sync::atomic::Ordering::SeqCst) != 0 {
                    receipt["evidence_basis"] = json!(43);
                }
                Ok(receipt)
            }
        }).err().unwrap();
        assert_eq!(
            error.status,
            if matches!(change, "work" | "run_association" | "malformed") {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::CONFLICT
            },
            "{change}: {error:?}"
        );
        assert!(state.inner.lock().unwrap().delegations.is_empty());
        assert!(
            error.message.contains(
                if matches!(change, "work" | "run_association" | "malformed") {
                    "malformed work/run identity"
                } else if change == "cut" {
                    "changed while criterion evidence was read"
                } else if change == "before_window" {
                    "different task, run"
                } else if change == "authority" {
                    "authority changed"
                } else {
                    "identity changed"
                }
            ),
            "{change}: {error:?}"
        );
    }
}

#[test]
fn criterion_evidence_request_closing_core_spends_the_original_deadline() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, page) = canonical_binding_receipts();
    install_binding_evidence_transport(&state, &parent, vec![page]);
    let start = std::time::Instant::now();
    let elapsed = AtomicUsize::new(0);
    let cores = AtomicUsize::new(0);
    let error = state
        .request_acceptance_evaluation_until(
            &parent,
            evaluation_request(None),
            |_, args, timeout| {
                assert!(timeout <= ENGRAM_WORK_BINDING_COMMAND_TIMEOUT);
                if args.iter().any(|arg| arg == "inspect") {
                    if cores.fetch_add(1, std::sync::atomic::Ordering::SeqCst) != 0 {
                        elapsed.store(61, std::sync::atomic::Ordering::SeqCst);
                    }
                    Ok(canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[], "omitted":0}))
                } else {
                    assert!(!args.iter().any(|arg| arg == "control-policy"));
                    Ok(show.clone())
                }
            },
            start + acceptance_evaluation_request_tracker_budget(),
            || {
                start
                    + Duration::from_secs(elapsed.load(std::sync::atomic::Ordering::SeqCst) as u64)
            },
        )
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("budget was spent"));
    assert_eq!(cores.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(state.inner.lock().unwrap().delegations.is_empty());
}

#[test]
fn criterion_evidence_request_selected_window_body_survives_the_final_entry_limit() {
    for count in [40, 41, 64] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        super::super::delegation_support::install_delegation_codex_runtime(
            &state,
            "selected-window-runtime",
        );
        let locator = "00000000000000000000000000000001";
        let proof = "Selected oldest window proof survives the actual brief boundary";
        let mut show = show_receipt(None);
        show["notes"] = json!((1..=count).map(|index| json!({
            "locator": format!("{index:032x}"), "kind": "note", "family": "notes",
            "summary": if index == 1 { proof.to_owned() } else { format!("Window proof {index}") }
        })).collect::<Vec<_>>());
        let request = serde_json::from_value(json!({"workRef":"w-task", "agent":"Codex",
            "criterionEvidence":[{"criterion":1,"locators":[locator]}]}))
        .unwrap();
        let response = state
            .request_acceptance_evaluation_with_runner(&parent, request, |_, args, _| {
                if args.iter().any(|arg| arg == "--note") {
                    Ok(json!({"work_ref":"w-task", "note":{"locator":locator,
                    "kind":"generic", "family":"notes", "summary":proof}}))
                } else if args.iter().any(|arg| arg == "inspect") {
                    Ok(canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full_receipt())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[],"omitted":0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["independent_session"])))
                } else {
                    Ok(show.clone())
                }
            })
            .unwrap();
        let wire = serde_json::to_value(response).unwrap();
        let inner = state.inner.lock().unwrap();
        let delegation = inner
            .delegations
            .iter()
            .find(|row| row.id == wire["delegation"]["id"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            delegation.prompt.matches(proof).count(),
            1,
            "{count}: {}",
            delegation.prompt
        );
        assert!(
            !wire["evidenceOmissions"]["leftOut"]["locators"]
                .as_array()
                .unwrap()
                .contains(&json!(locator)),
            "{count}: selected body was carried"
        );
        assert!(delegation.prompt.contains(&format!("Window proof {count}")));
    }
}

#[test]
fn criterion_evidence_final_body_plan_recomputes_clips_and_omissions() {
    let (show, full, _) = canonical_binding_receipts();
    let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
    let locator = "cccccccccccccccccccccccccccccccc".to_owned();
    let proof = format!(
        "Selected full proof {} end-of-selected-proof",
        "detail ".repeat(1000)
    );
    let mut exact = task.evidence[0].clone();
    exact.locator = locator.clone();
    exact.summary = Some(proof);
    exact.verification = None;
    let mut partial = exact.clone();
    partial.summary = Some("Tracker partial proof".to_owned());
    partial.cut_by_tracker = true;
    task.evidence.insert(0, partial);
    task.indexed_evidence.push(exact);
    for criterion in [1, 2] {
        task.criterion_evidence.push(AcceptanceCriterionEvidence {
            criterion,
            locators: vec![locator.clone()],
            association: "requester".to_owned(),
        });
    }
    let supplied =
        render_acceptance_evaluator_brief(&task, ".", 2, 1, MAX_ACCEPTANCE_BRIEF_OUTCOME_BYTES);
    assert_eq!(supplied.prompt.matches("end-of-selected-proof").count(), 1);
    assert!(!supplied.cuts.left_out.contains(&locator));
    assert!(!supplied.cuts.clipped.contains(&locator));
    assert!(!supplied.cuts.cut_by_tracker.contains(&locator));
    assert_eq!(
        acceptance_same_session_brief_cuts(&task)
            .left_out
            .iter()
            .filter(|value| **value == locator)
            .count(),
        1
    );
    // A later fitting candidate drops the index; it must forget the earlier
    // complete body and report its omission once, while preserving newer proof.
    let floor = render_acceptance_evaluator_brief_with_index_detail(
        &task,
        ".",
        "",
        2,
        0,
        MAX_ACCEPTANCE_BRIEF_OUTCOME_BYTES,
        AcceptanceOmissionDetail::Full,
        AcceptanceOmissionDetail::Minimal,
        AcceptanceOmissionDetail::Minimal,
    );
    let fitted = build_acceptance_evaluator_brief(&task, ".", floor.prompt.len()).unwrap();
    assert!(!fitted.prompt.contains("end-of-selected-proof"));
    assert!(fitted.prompt.contains("A newer test failed"));
    assert!(!fitted.cuts.left_out.contains(&locator));
    assert_eq!(
        fitted
            .cuts
            .cut_by_tracker
            .iter()
            .filter(|value| **value == locator)
            .count(),
        1
    );
    assert!(
        fitted
            .prompt
            .contains("where a verdict depends on them, give insufficient-evidence")
    );
    assert!(!fitted.prompt.contains("criterionEvidence in the response"));
}

#[test]
fn criterion_evidence_request_legacy_and_absent_identity_are_explicitly_unavailable() {
    for absence in ["legacy", "no_active_run", "unsupported", "malformed"] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (mut show, full, _) = canonical_binding_receipts();
        if absence == "no_active_run" {
            show.as_object_mut().unwrap().remove("evidence_basis");
        }
        let transport = install_binding_evidence_transport(&state, &parent, vec![]);
        let response = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |_, args, _| {
                if args.iter().any(|arg| arg == "inspect") {
                    if absence == "unsupported" {
                        return Err(EngramTransportError::remote(EngramControlErrorBody {
                            code: "unsupported_operation".to_owned(),
                            message: "core identity read unsupported".to_owned(),
                        }));
                    }
                    let mut core = canonical_core_receipt();
                    if absence == "legacy" {
                        return Ok(show.clone());
                    }
                    if absence == "no_active_run" {
                        core["status"]["work"]["active_run_id"] = Value::Null;
                        // Faithful core keeps the latest same-work historical run.
                        // It must never be adopted as the active run.
                    } else if absence == "malformed" {
                        return Ok(json!({}));
                    }
                    Ok(core)
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[], "omitted":0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["same_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        );
        if absence == "malformed" {
            let error = response.err().unwrap();
            assert_eq!(error.status, StatusCode::BAD_GATEWAY);
            assert!(error.message.contains("malformed work/run identity"));
        } else {
            let error = response
                .err()
                .expect("missing source identity or active evaluation run cannot mint a token");
            assert_eq!(error.status, StatusCode::CONFLICT, "{absence}: {error:?}");
            assert!(
                error.message.contains(if absence == "no_active_run" {
                    "no acceptance and evidence basis"
                } else {
                    "canonical work identity and source authority are unavailable"
                }),
                "{absence}: {error:?}"
            );
        }
        assert!(transport.requests.lock().unwrap().is_empty());
        assert!(state.inner.lock().unwrap().delegations.is_empty());
    }
}

#[test]
fn criterion_evidence_request_carries_old_proof_outside_the_notes_window() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let request = serde_json::from_value(json!({
        "workRef": "w-task",
        "criterionEvidence": [{"criterion": 1, "locators": ["cccccccccccccccccccccccccccccccc"]}]
    }))
    .expect("explicit criterion evidence is a supported request field");
    let mut show = show_receipt(Some("same_session"));
    show["notes_omitted"] = json!(71);
    let calls: RecordedEngramCalls = Arc::default();
    let seen = calls.clone();
    let response = state
        .request_acceptance_evaluation_with_runner(&parent, request, move |connection, args, _| {
            seen.lock()
                .unwrap()
                .push((connection.clone(), args.to_vec()));
            if args.iter().any(|arg| arg == "--note") {
                Ok(json!({"work_ref": "w-task", "note": {
                    "locator": "cccccccccccccccccccccccccccccccc", "kind": "generic", "family": "notes",
                    "summary": "Only older proof: the route answers with the expected body.",
                    "body_bytes": 65, "non_holder": false
                }}))
            } else if args.iter().any(|arg| arg == "inspect") { Ok(canonical_core_receipt()) } else if args.iter().any(|arg| arg == "--full") {
                Ok(full_receipt())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items": [], "omitted": 0}))
            } else if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&["same_session"])))
            } else {
                Ok(show.clone())
            }
        })
        .unwrap();
    let wire = serde_json::to_value(response).unwrap();
    let brief = wire["brief"].as_str().unwrap();
    assert!(
        brief.contains("Criterion 1: cccccccccccccccccccccccccccccccc"),
        "{brief}"
    );
    assert!(
        brief.contains("Only older proof: the route answers with the expected body."),
        "{brief}"
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "--note"))
            .count(),
        1
    );
    assert_eq!(
        wire["criterionEvidence"][0]["locators"],
        json!(["cccccccccccccccccccccccccccccccc"])
    );
    assert!(
        !wire["evidenceOmissions"]["leftOut"]["locators"]
            .as_array()
            .unwrap()
            .contains(&json!("cccccccccccccccccccccccccccccccc")),
        "the indexed proof body is carried"
    );
}

#[test]
fn criterion_evidence_request_refuses_invalid_positions_before_tracker_io() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let request = serde_json::from_value(json!({
        "workRef": "w-task",
        "criterionEvidence": [{"criterion": 0, "locators": ["cccccccccccccccccccccccccccccccc"]}]
    }))
    .expect("the field is decoded before validation");
    let error = state
        .request_acceptance_evaluation_with_runner(&parent, request, |_, _, _| {
            panic!("invalid request must not read the tracker")
        })
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
}

#[test]
fn criterion_evidence_request_independent_brief_indexes_scattered_older_notes() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "criterion-evidence-runtime",
    );
    let request = serde_json::from_value(json!({"workRef": "w-task", "agent": "Codex",
    "criterionEvidence": [
        {"criterion": 1, "locators": ["cccccccccccccccccccccccccccccccc", "dddddddddddddddddddddddddddddddd"]},
        {"criterion": 2, "locators": ["dddddddddddddddddddddddddddddddd"]}
    ]}))
    .unwrap();
    let mut show = show_receipt(None);
    show["notes_omitted"] = json!(80);
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    let response = state.request_acceptance_evaluation_with_runner(&parent, request, move |_, args, _| {
        if let Some(position) = args.iter().position(|arg| arg == "--note") {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let locator = &args[position + 1];
            Ok(json!({"work_ref": "w-task", "note": {"locator": locator, "kind": "generic", "family": "notes",
                "summary": if locator == "cccccccccccccccccccccccccccccccc" {"Older route implementation proof"} else {"Separate older request test proof"}}}))
        } else if args.iter().any(|arg| arg == "inspect") { Ok(canonical_core_receipt()) } else if args.iter().any(|arg| arg == "--full") { Ok(full_receipt()) }
        else if args.iter().any(|arg| arg == "held") { Ok(json!({"items": [], "omitted": 0})) }
        else if args.first().map(String::as_str) == Some("control-policy") { Ok(policy_receipt(Some(&["independent_session"]))) }
        else { Ok(show.clone()) }
    }).unwrap();
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "shared proof is read only once"
    );
    assert_eq!(
        compact_acceptance_evaluation_request_result(&wire)["criterionEvidence"],
        wire["criterionEvidence"]
    );
    let inner = state.inner.lock().unwrap();
    let delegation = inner
        .delegations
        .iter()
        .find(|row| row.id == wire["delegation"]["id"].as_str().unwrap())
        .unwrap();
    let prompt = &delegation.prompt;
    assert!(
        prompt.contains("Criterion 1: cccccccccccccccccccccccccccccccc, dddddddddddddddddddddddddddddddd (requester)"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Criterion 2: dddddddddddddddddddddddddddddddd (requester)"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Older route implementation proof")
            && prompt.contains("Separate older request test proof")
    );
    assert_eq!(
        prompt.matches("Older route implementation proof").count(),
        1
    );
    assert_eq!(
        prompt.matches("Separate older request test proof").count(),
        1
    );
    assert!(
        prompt.find("Criterion evidence index").unwrap()
            < prompt.find("Evidence recorded").unwrap()
    );
    assert_eq!(delegation.write_policy, DelegationWritePolicy::ReadOnly);
}

#[test]
fn criterion_evidence_request_refuses_wrong_or_partial_records_and_changed_basis() {
    for failure in [
        "wrong_work",
        "wrong_locator",
        "non_holder",
        "body_omitted",
        "summary_truncated",
        "changed_cut",
        "changed_revision",
        "changed_ref",
        "changed_run",
        "changed_work_id",
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let request = serde_json::from_value(json!({"workRef": "w-task", "criterionEvidence": [
            {"criterion": 1, "locators": ["cccccccccccccccccccccccccccccccc"]}]}))
        .unwrap();
        let exact_read = AtomicUsize::new(0);
        let error = state.request_acceptance_evaluation_with_runner(&parent, request, |_, args, _| {
            if args.iter().any(|arg| arg == "--note") {
                exact_read.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut receipt = json!({"work_ref": "w-task", "note": {"locator": "cccccccccccccccccccccccccccccccc", "kind": "generic", "family": "notes", "summary": "Older proof"}});
                match failure {
                    "wrong_work" => receipt["work_ref"] = json!("w-other"),
                    "wrong_locator" => receipt["note"]["locator"] = json!("dddddddddddddddddddddddddddddddd"),
                    "non_holder" | "body_omitted" | "summary_truncated" => receipt["note"][failure] = json!(true),
                    _ => {}
                }
                Ok(receipt)
            } else if args.iter().any(|arg| arg == "inspect") { Ok(canonical_core_receipt()) } else if args.iter().any(|arg| arg == "--full") { Ok(full_receipt()) }
            else if args.iter().any(|arg| arg == "held") { Ok(json!({"items":[], "omitted":0})) }
            else {
                assert!(!args.iter().any(|arg| arg == "control-policy"), "no evaluator admission reads after invalid proof");
                let mut show = show_receipt(Some("same_session"));
                if exact_read.load(std::sync::atomic::Ordering::SeqCst) != 0 {
                    match failure {
                        "changed_cut" => show["evidence_basis"] = json!(43),
                        "changed_revision" => show["acceptance_basis"] = json!(8),
                        "changed_ref" => show["status"]["work"]["short_ref"] = json!("w-other"),
                        "changed_run" => show["status"]["work"]["active_run_id"] = json!("replacement-run"),
                        "changed_work_id" => show["status"]["work"]["work_id"] = json!("replacement-work"),
                        _ => {}
                    }
                }
                Ok(show)
            }
        }).err().unwrap();
        let (status, reason) = match failure {
            "wrong_work" => (StatusCode::BAD_GATEWAY, "belongs to another task"),
            "changed_ref" | "changed_run" | "changed_work_id" => {
                (StatusCode::CONFLICT, "canonical work/run identity changed")
            }
            "changed_cut" | "changed_revision" => (
                StatusCode::CONFLICT,
                "changed while criterion evidence was read",
            ),
            _ => (
                StatusCode::CONFLICT,
                "not returned whole as a citable record",
            ),
        };
        assert_eq!(error.status, status, "{failure}: {}", error.message);
        assert!(
            error.message.contains(reason),
            "{failure}: {}",
            error.message
        );
        assert!(state.inner.lock().unwrap().delegations.is_empty());
    }
}

struct BindingEvidenceTransport {
    pages: Mutex<std::collections::VecDeque<Value>>,
    requests: Mutex<Vec<Value>>,
}

impl EngramControlTransport for BindingEvidenceTransport {
    fn request(
        &self,
        _: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _: Duration,
    ) -> Result<Value, EngramTransportError> {
        assert!(matches!(
            request,
            EngramControlRequest::AcceptanceBindingRead { .. }
        ));
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_value(request).unwrap());
        let page = self
            .pages
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra binding page");
        if let Some(error) = page.get("fixture_error") {
            return Err(EngramTransportError::remote(EngramControlErrorBody {
                code: error["code"].as_str().unwrap().to_owned(),
                message: error["message"]
                    .as_str()
                    .unwrap_or("scripted refusal")
                    .to_owned(),
            }));
        }
        Ok(page)
    }

    fn shutdown_session(&self, _: &str) {}
}

fn install_binding_evidence_transport(
    state: &AppState,
    parent: &str,
    pages: Vec<Value>,
) -> Arc<BindingEvidenceTransport> {
    let transport = Arc::new(BindingEvidenceTransport {
        pages: Mutex::new(pages.into()),
        requests: Mutex::new(Vec::new()),
    });
    state.install_test_engram_transport(transport.clone());
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(parent).unwrap();
    let project = inner.sessions[index].session.project_id.clone().unwrap();
    inner
        .projects
        .iter_mut()
        .find(|row| row.id == project)
        .unwrap()
        .engram
        .as_mut()
        .unwrap()
        .turn_gated_control = true;
    inner.sessions[index].engram.routing_token = Some("criterion-read-token".to_owned());
    transport
}

// Faithful host-core shape at the pinned producer: identities are on core
// inspect, never injected into the safe agent show projections.
pub(super) fn canonical_core_receipt() -> Value {
    json!({"status":{"work":{"short_ref":"w-task", "revision":7,
        "work_id":"01a0b6c5-4b4f-7b41-9e32-edc597077acf",
        "active_run_id":"01a0b6c5-4b4f-7b41-9e32-edd7dea8b369"}},
        "run":{"work_id":"01a0b6c5-4b4f-7b41-9e32-edc597077acf",
            "run_id":"01a0b6c5-4b4f-7b41-9e32-edd7dea8b369",
            "root_execution_id":"01a0b6c5-4b4f-7b41-9e32-edd7dea8b300", "generation":1},
        "control_binding":null})
}

fn canonical_binding_receipts() -> (Value, Value, Value) {
    let mut show = show_receipt(Some("same_session"));
    let mut full = full_receipt();
    full["work"]["acceptance_bindings"] =
        json!([{"criterion": 2, "requirement": {"check_kind": "test"}}]);
    show["notes_omitted"] = json!(90);
    show["notes"] = json!([{"locator": "ffffffff6666", "kind": "verification", "family": "notes", "summary": "A newer test failed",
        "verification": {"check_kind": "test", "result": "failed", "source_revision": "git:newer"}}]);
    let page = json!({"basis": {"project_id": "established-project", "work_id": "01a0b6c5-4b4f-7b41-9e32-edc597077acf", "work_revision": 7,
        "run_id": "01a0b6c5-4b4f-7b41-9e32-edd7dea8b369", "run_cut": 42}, "total": 2, "earlier": 0, "shown": 2, "omitted": 0,
        "continuation": null, "rows": [{"criterion": 1, "binding": null}, {"criterion": 2, "binding": {
            "requirement": {"check_kind": "test"}, "obligation": {
                "obligation_id": "01a0b6c5-4b4f-7b41-9e32-edc597077aff", "definition": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "work_revision": 7, "rule": {"rule_id": "acceptance_criterion_requires_verification:2", "rule_version": 1},
                "triggering_observation": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "trigger_position": 2, "definition_position": 3,
                "state": "satisfied", "resolution": {"record": "dddddddddddddddddddddddddddddddd", "position": 35, "kind": "satisfied",
                    "satisfaction": {"evaluated_cut": 34, "verification": {
                        "record": "cccccccccccccccccccccccccccccccc", "position": 33, "check_kind": "test",
                        "check_fingerprint": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "result": "passed",
                        "source_basis": {"workspace_id": "fixture-workspace", "source_revision": "git:older"},
                        "producer": {"record": "abababababababababababababababab", "position": 32, "outcome": "succeeded"}}}}}}}]});
    (show, full, page)
}

#[test]
fn criterion_evidence_request_discovers_original_bound_check_outside_the_window() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, page) = canonical_binding_receipts();
    let transport = install_binding_evidence_transport(&state, &parent, vec![page]);
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |_, args, _| {
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items": [], "omitted": 0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["same_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        )
        .unwrap();
    let wire = serde_json::to_value(response).unwrap();
    let brief = wire["brief"].as_str().unwrap();
    assert!(
        brief.contains(
            "Criterion 2: cccccccccccccccccccccccccccccccc (original obligation closure)"
        ),
        "{brief}"
    );
    assert!(
        brief.contains("verification test passed at git:older"),
        "{brief}"
    );
    assert!(
        brief.contains("git:older") && brief.contains("not current freshness"),
        "{brief}"
    );
    assert!(
        brief.contains("ffffffff6666 (verification test failed at git:newer)"),
        "{brief}"
    );
    assert!(!brief.contains("carries no evidence bodies"));
    let newest = brief
        .find("ffffffff6666 (verification test failed at git:newer)")
        .unwrap();
    let original = brief.find("verification test passed at git:older").unwrap();
    assert!(
        newest < original,
        "the original closure must not become the newest verification: {brief}"
    );
    assert_eq!(
        wire["evidenceOmissions"]["leftOut"]["locators"],
        json!(["ffffffff6666"])
    );
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["operation"], "acceptance_binding_read");
    assert_eq!(requests[0]["routing_token"], "criterion-read-token");
    assert_eq!(requests[0]["expected_work_revision"], 7);
    assert!(
        requests[0].get("run_cut").is_none(),
        "the first page captures the cut"
    );
}

#[test]
fn criterion_evidence_request_refuses_changed_canonical_basis_and_malformed_closure() {
    for failure in [
        "run",
        "cut",
        "revision",
        "rule",
        "result",
        "producer",
        "count",
        "missing_continuation",
        "open_trigger",
        "waived_trigger",
        "displaced_trigger",
        "waived_resolution",
        "displaced_resolution",
        "equal_producer",
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, full, mut page) = canonical_binding_receipts();
        match failure {
            "run" => page["basis"]["run_id"] = json!("other-run"),
            "cut" => page["basis"]["run_cut"] = json!(43),
            "revision" => page["basis"]["work_revision"] = json!(8),
            "rule" => page["rows"][1]["binding"]["obligation"]["rule"]["rule_version"] = json!(2),
            "result" => {
                page["rows"][1]["binding"]["obligation"]["resolution"]["satisfaction"]["verification"]
                    ["result"] = json!("failed")
            }
            "producer" => {
                page["rows"][1]["binding"]["obligation"]["resolution"]["satisfaction"]["verification"]
                    ["producer"]["outcome"] = json!("failed")
            }
            "count" => page["earlier"] = json!(u64::MAX),
            "missing_continuation" => {
                page["rows"].as_array_mut().unwrap().pop();
                page["shown"] = json!(1);
                page["omitted"] = json!(1);
            }
            "open_trigger"
            | "waived_trigger"
            | "displaced_trigger"
            | "waived_resolution"
            | "displaced_resolution" => {
                let obligation = &mut page["rows"][1]["binding"]["obligation"];
                let kind = failure.split('_').next().unwrap();
                obligation["state"] = json!(kind);
                if kind == "open" {
                    obligation["resolution"] = Value::Null;
                } else {
                    obligation["resolution"]["kind"] = json!(kind);
                    obligation["resolution"]["satisfaction"] = Value::Null;
                }
                if failure.ends_with("trigger") {
                    obligation["trigger_position"] = obligation["definition_position"].clone();
                } else {
                    obligation["resolution"]["position"] =
                        obligation["definition_position"].clone();
                }
            }
            "equal_producer" => {
                let verification = &mut page["rows"][1]["binding"]["obligation"]["resolution"]["satisfaction"]
                    ["verification"];
                verification["producer"]["position"] = verification["position"].clone();
            }
            _ => unreachable!(),
        }
        let transport = install_binding_evidence_transport(&state, &parent, vec![page]);
        let error = state
            .request_acceptance_evaluation_with_runner(
                &parent,
                evaluation_request(None),
                |_, args, _| {
                    if args.iter().any(|arg| arg == "inspect") {
                        Ok(canonical_core_receipt())
                    } else if args.iter().any(|arg| arg == "--full") {
                        Ok(full.clone())
                    } else if args.iter().any(|arg| arg == "held") {
                        Ok(json!({"items":[], "omitted":0}))
                    } else {
                        assert!(!args.iter().any(|arg| arg == "control-policy"));
                        Ok(show.clone())
                    }
                },
            )
            .err()
            .unwrap();
        let (status, reason) = match failure {
            "run" | "cut" | "revision" | "count" | "missing_continuation" => (
                StatusCode::CONFLICT,
                "different task, run, revision, cut or page boundary",
            ),
            _ => (
                StatusCode::BAD_GATEWAY,
                "invalid canonical obligation or verification",
            ),
        };
        assert_eq!(error.status, status, "{failure}: {}", error.message);
        assert!(
            error.message.contains(reason),
            "{failure}: {}",
            error.message
        );
        assert!(state.inner.lock().unwrap().delegations.is_empty());
        assert_eq!(
            transport.requests.lock().unwrap().len(),
            1,
            "{failure} must reach the malformed page"
        );
    }
}

#[test]
fn criterion_evidence_request_pins_all_closure_pages_before_admission() {
    for changed_cut in [false, true] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, full, page) = canonical_binding_receipts();
        let mut first = page.clone();
        first["rows"] = json!([page["rows"][0]]);
        first["shown"] = json!(1);
        first["omitted"] = json!(1);
        first["continuation"] = json!("opaque-binding-page");
        let mut last = page.clone();
        last["rows"] = json!([page["rows"][1]]);
        last["earlier"] = json!(1);
        last["shown"] = json!(1);
        if changed_cut {
            last["basis"]["run_cut"] = json!(43);
        }
        let transport = install_binding_evidence_transport(&state, &parent, vec![first, last]);
        let response = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |_, args, _| {
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items": [], "omitted": 0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    assert!(
                        !changed_cut,
                        "a moving index cannot reach evaluator admission"
                    );
                    Ok(policy_receipt(Some(&["same_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        );
        if changed_cut {
            assert_eq!(response.err().unwrap().status, StatusCode::CONFLICT);
        } else {
            let wire = serde_json::to_value(response.unwrap()).unwrap();
            assert_eq!(
                wire["criterionEvidence"][0]["locators"],
                json!(["cccccccccccccccccccccccccccccccc"])
            );
        }
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].get("after").is_none());
        assert_eq!(requests[1]["after"], "opaque-binding-page");
        assert!(
            requests
                .iter()
                .all(|request| request["expected_work_revision"] == 7
                    && request.get("run_cut").is_none())
        );
    }
}

#[test]
fn criterion_evidence_and_carried_failure_survive_the_same_brief() {
    let (show, mut full, page) = canonical_binding_receipts();
    let failed = "5bc7bccd9ef84c4f810696e190d65ce9";
    full["work"]["evaluation"] = json!({
        "hash": failed, "mode": "independent_session", "work_revision": 2,
        "carried_failure": {
            "evaluation": failed, "revised_by": "planner", "judged_revision": 2,
            "failing": 1, "supersedes_required": false,
            "judged_criteria": ["The old route exists everywhere"],
            "blocking": [{"criterion": 1, "verdict": "fail",
                "rationale": "Only one route exists."}],
            "judged_bindings": []
        }
    });
    let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
    apply_acceptance_binding_evidence(
        &mut task,
        serde_json::from_value::<AcceptanceBindingEvidencePage>(page)
            .unwrap()
            .rows,
    )
    .unwrap();
    let independent = build_acceptance_evaluator_brief(&task, ".", MAX_ACCEPTANCE_BRIEF_BYTES)
        .unwrap()
        .prompt;
    let same_session =
        build_same_session_acceptance_brief(&task, None, MAX_ACCEPTANCE_BRIEF_BYTES).unwrap();
    for prompt in [&independent, &same_session] {
        let carried = prompt
            .find(&format!("Carried failure: evaluation {failed}"))
            .unwrap();
        assert!(
            task.criteria
                .iter()
                .all(|criterion| prompt.find(criterion).unwrap() < carried)
        );
        assert!(prompt.contains("Only one route exists."));
        assert!(prompt.contains("cccccccccccccccccccccccccccccccc"));
        assert!(prompt.contains("original obligation closure"));
        assert!(prompt.contains("does not prove that the check is current"));
    }
    assert!(same_session.contains(&format!(", supersedes {failed}, and exactly one verdict")));
}

#[test]
fn criterion_evidence_request_keeps_base_enabled_evaluation_without_control_discovery() {
    for missing in ["control_disabled", "routing_token"] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, full, page) = canonical_binding_receipts();
        let transport = install_binding_evidence_transport(&state, &parent, vec![page]);
        {
            let mut inner = state.inner.lock().unwrap();
            if missing == "control_disabled" {
                inner
                    .projects
                    .iter_mut()
                    .find(|row| row.id == project)
                    .unwrap()
                    .engram
                    .as_mut()
                    .unwrap()
                    .turn_gated_control = false;
            } else {
                let index = inner.find_session_index(&parent).unwrap();
                inner.sessions[index].engram.routing_token = None;
            }
        }
        let response = state
            .request_acceptance_evaluation_with_runner(
                &parent,
                evaluation_request(None),
                |_, args, _| {
                    if args.iter().any(|arg| arg == "inspect") {
                        Ok(canonical_core_receipt())
                    } else if args.iter().any(|arg| arg == "--full") {
                        Ok(full.clone())
                    } else if args.iter().any(|arg| arg == "held") {
                        Ok(json!({"items": [], "omitted": 0}))
                    } else if args.first().map(String::as_str) == Some("control-policy") {
                        Ok(policy_receipt(Some(&["same_session"])))
                    } else {
                        Ok(show.clone())
                    }
                },
            )
            .unwrap_or_else(|error| panic!("{missing}: {error:?}"));
        let wire = serde_json::to_value(response).unwrap();
        assert!(
            wire["brief"]
                .as_str()
                .unwrap()
                .contains("canonical index unavailable")
        );
        assert!(
            wire["criterionEvidence"][0]["association"]
                .as_str()
                .unwrap()
                .contains("unavailable")
        );
        assert!(transport.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn criterion_evidence_large_index_keeps_newer_failed_window_body() {
    let (show, full, _) = canonical_binding_receipts();
    let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
    let current = "A newer failed check still needs assessment";
    task.evidence[0].summary = Some(current.to_owned());
    // The request path may read sixteen older records of up to 16 KiB each.
    let mut locators = Vec::new();
    for index in 0..16 {
        let locator = format!("{index:032x}");
        let mut proof = task.evidence[0].clone();
        proof.locator = locator.clone();
        proof.summary = Some("Older selected proof ".repeat(700));
        task.indexed_evidence.push(proof);
        locators.push(locator);
    }
    task.criterion_evidence.push(AcceptanceCriterionEvidence {
        criterion: 1,
        locators,
        association: "requester".to_owned(),
    });
    let brief = build_acceptance_evaluator_brief(&task, ".", 12_000).unwrap();
    assert!(
        brief.prompt.contains(current),
        "newer failed evidence must survive optional indexed bodies: {}",
        brief.prompt
    );
    assert!(!brief.cuts.left_out.contains(&"ffffffff6666".to_owned()));
}

#[test]
fn criterion_evidence_request_preserves_criteria_and_truthful_cuts_when_details_shrink() {
    let (show, full, page) = canonical_binding_receipts();
    let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
    let rows = serde_json::from_value::<AcceptanceBindingEvidencePage>(page)
        .unwrap()
        .rows;
    apply_acceptance_binding_evidence(&mut task, rows).unwrap();
    task.indexed_evidence
        .iter_mut()
        .find(|row| row.locator == "cccccccccccccccccccccccccccccccc")
        .unwrap()
        .summary = Some("proof ".repeat(400));
    let same_minimal = render_same_session_acceptance_brief_with_detail(
        &task,
        None,
        AcceptanceOmissionDetail::Minimal,
    )
    .len();
    let (brief, cuts) =
        build_same_session_acceptance_brief_and_cuts(&task, None, same_minimal).unwrap();
    assert!(brief.contains("Criterion evidence details not shown"));
    assert!(
        cuts.left_out
            .contains(&"cccccccccccccccccccccccccccccccc".to_owned())
    );
    assert!(
        task.criteria
            .iter()
            .all(|criterion| brief.contains(criterion))
    );
    let floor = render_acceptance_evaluator_brief_with_details(
        &task,
        ".",
        "",
        0,
        0,
        0,
        AcceptanceOmissionDetail::Minimal,
        AcceptanceOmissionDetail::Minimal,
    )
    .prompt
    .len();
    let brief = build_acceptance_evaluator_brief(&task, ".", floor).unwrap();
    assert!(
        brief
            .prompt
            .contains("Criterion evidence details not shown")
    );
    assert!(
        brief
            .cuts
            .left_out
            .contains(&"cccccccccccccccccccccccccccccccc".to_owned())
    );
    assert!(
        task.criteria
            .iter()
            .all(|criterion| brief.prompt.contains(criterion))
    );
    assert!(build_acceptance_evaluator_brief(&task, ".", floor - 1).is_err());
}

#[test]
fn criterion_evidence_request_discovery_deadline_refuses_without_a_partial_evaluation() {
    let (mut show, full, _) = canonical_binding_receipts();
    show["status"]["work"]["work_id"] = Value::Null;
    show["status"]["work"]["active_run_id"] = Value::Null;
    let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
    let links = vec![AcceptanceCriterionEvidenceRequest {
        criterion: 1,
        locators: vec!["cccccccccccccccccccccccccccccccc".to_owned()],
    }];
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let connection = state
        .work_read_snapshot(&project, None)
        .unwrap()
        .1
        .unwrap()
        .connection;
    let start = std::time::Instant::now();
    let calls = AtomicUsize::new(0);
    let now =
        || start + Duration::from_secs(61 * calls.load(std::sync::atomic::Ordering::SeqCst) as u64);
    let error = read_requested_acceptance_evidence(&mut task, &links, &connection,
        &["work", "show", "w-task", "--notes", "--gates", "--json"].map(str::to_owned), &|_, _, timeout| {
            assert!(timeout <= ENGRAM_WORK_BINDING_COMMAND_TIMEOUT);
            assert_eq!(calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0, "no read after the discovery deadline");
            Ok(json!({"work_ref": "w-task", "note": {"locator": "cccccccccccccccccccccccccccccccc", "family": "notes", "summary": "proof"}}))
        }, start + ACCEPTANCE_CRITERION_EVIDENCE_READ_BUDGET, &now).err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("budget was spent"));
}

fn request_canonical_fixture(
    state: &AppState,
    parent: &str,
    show: &Value,
    full: &Value,
) -> Result<AcceptanceEvaluationRequestResponse, ApiError> {
    state.request_acceptance_evaluation_with_runner(
        parent,
        evaluation_request(None),
        |_, args, _| {
            if args.iter().any(|arg| arg == "inspect") {
                Ok(canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items": [], "omitted": 0}))
            } else if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&["same_session"])))
            } else {
                Ok(show.clone())
            }
        },
    )
}

#[test]
fn criterion_evidence_remote_refusals_distinguish_unsupported_movement_and_protocol() {
    for code in [
        "unsupported_operation",
        "invalid_request",
        "acceptance_binding_read_wrong_revision",
        "acceptance_binding_read_wrong_run",
        "acceptance_binding_read_stale_cut",
        "control_session_not_bound",
        "control_session_token_mismatch",
        "control_connection_superseded",
        "invalid_control_session",
        "storage_integrity",
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, full, _) = canonical_binding_receipts();
        let message = if code == "invalid_request" {
            "unknown variant `acceptance_binding_read`, expected session_bind or turn_begin"
        } else {
            "scripted refusal"
        };
        let transport = install_binding_evidence_transport(
            &state,
            &parent,
            vec![json!({"fixture_error": {"code": code, "message": message}})],
        );
        let response = request_canonical_fixture(&state, &parent, &show, &full);
        if matches!(code, "unsupported_operation" | "invalid_request") {
            let wire = serde_json::to_value(response.unwrap()).unwrap();
            assert!(
                wire["criterionEvidence"][0]["association"]
                    .as_str()
                    .unwrap()
                    .contains("does not support")
            );
            assert!(
                wire["criterionEvidence"][0]["locators"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        } else {
            let error = response.err().unwrap();
            let movement = matches!(
                code,
                "acceptance_binding_read_wrong_revision"
                    | "acceptance_binding_read_wrong_run"
                    | "acceptance_binding_read_stale_cut"
                    | "control_session_not_bound"
                    | "control_session_token_mismatch"
                    | "control_connection_superseded"
                    | "invalid_control_session"
            );
            assert_eq!(
                error.status,
                if movement {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::BAD_GATEWAY
                },
                "{code}: {error:?}"
            );
            assert!(error.message.contains(if movement {
                "request a new evaluation"
            } else {
                "acceptance_binding_read"
            }));
        }
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
        assert!(state.inner.lock().unwrap().delegations.is_empty());
    }
}

#[test]
fn criterion_evidence_open_waived_and_displaced_never_associate_a_pass() {
    for kind in ["open", "waived", "displaced"] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, full, mut page) = canonical_binding_receipts();
        let obligation = &mut page["rows"][1]["binding"]["obligation"];
        obligation["state"] = json!(kind);
        if kind == "open" {
            obligation["resolution"] = Value::Null;
        } else {
            obligation["resolution"]["kind"] = json!(kind);
            obligation["resolution"]["satisfaction"] = Value::Null;
        }
        install_binding_evidence_transport(&state, &parent, vec![page]);
        let wire =
            serde_json::to_value(request_canonical_fixture(&state, &parent, &show, &full).unwrap())
                .unwrap();
        assert!(
            wire["criterionEvidence"].as_array().unwrap().is_empty(),
            "{kind}: {wire}"
        );
        assert!(!wire["brief"].as_str().unwrap().contains("git:older"));
    }
}

#[test]
fn criterion_evidence_validation_requires_full_ids_and_bounded_selection() {
    for locator in [
        "cccccccc",
        "cCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
        "cccccccccccccccccccccccccccccccc:1",
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let request = serde_json::from_value(json!({"workRef": "w-task", "criterionEvidence": [{"criterion": 1, "locators": [locator]}]})).unwrap();
        let error = state
            .request_acceptance_evaluation_with_runner(&parent, request, |_, _, _| {
                panic!("invalid locator must not read the tracker")
            })
            .err()
            .unwrap();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("full 32- or 64-character"));
    }
    let links = vec![AcceptanceCriterionEvidenceRequest {
        criterion: 1,
        locators: (0..17).map(|index| format!("{index:032x}")).collect(),
    }];
    assert_eq!(
        validate_acceptance_criterion_evidence_request(&links)
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert!(
        validate_acceptance_criterion_evidence_request(&[AcceptanceCriterionEvidenceRequest {
            criterion: 1,
            locators: vec!["c".repeat(64)]
        }])
        .is_ok()
    );
    let schema = acceptance_evaluation_request_tool_definition();
    assert_eq!(
        schema["inputSchema"]["properties"]["criterionEvidence"]["maxItems"],
        16
    );
    assert_eq!(
        schema["inputSchema"]["properties"]["criterionEvidence"]["items"]["properties"]["locators"]
            ["items"]["pattern"],
        "^([a-f0-9]{32}|[a-f0-9]{64})$"
    );
}

#[test]
fn criterion_evidence_page_limits_refuse_before_admission() {
    for failure in [
        "empty_cursor",
        "oversize_cursor",
        "repeated_cursor",
        "oversize_page",
        "page_exhaustion",
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let (show, mut full, page) = canonical_binding_receipts();
        let mut first = page.clone();
        first["rows"] = json!([page["rows"][0]]);
        first["shown"] = json!(1);
        first["omitted"] = json!(1);
        first["continuation"] = json!("cursor-a");
        let pages = match failure {
            "empty_cursor" => {
                first["continuation"] = json!("");
                vec![first]
            }
            "oversize_cursor" => {
                first["continuation"] = json!("a".repeat(4097));
                vec![first]
            }
            "oversize_page" => {
                first["rows"][0]["binding"] = json!({"padding": "a".repeat(16 * 1024)});
                vec![first]
            }
            "repeated_cursor" => {
                full["work"]["acceptance"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("Third criterion"));
                first["total"] = json!(3);
                first["omitted"] = json!(2);
                let mut second = first.clone();
                second["rows"] = json!([page["rows"][1]]);
                second["earlier"] = json!(1);
                second["omitted"] = json!(1);
                vec![first, second]
            }
            "page_exhaustion" => {
                full["work"]["acceptance"] = json!(
                    (1..=129)
                        .map(|i| format!("Criterion {i}"))
                        .collect::<Vec<_>>()
                );
                (0..16)
                    .map(|i| {
                        let mut chunk = page.clone();
                        chunk["total"] = json!(129);
                        chunk["earlier"] = json!(i * 8);
                        chunk["shown"] = json!(8);
                        chunk["omitted"] = json!(129 - (i + 1) * 8);
                        chunk["continuation"] = json!(format!("cursor-{i}"));
                        chunk["rows"] =
                            json!((1..=8)
                            .map(|offset| json!({"criterion": i * 8 + offset, "binding": null}))
                            .collect::<Vec<_>>());
                        chunk
                    })
                    .collect()
            }
            _ => unreachable!(),
        };
        let transport = install_binding_evidence_transport(&state, &parent, pages);
        let error = state
            .request_acceptance_evaluation_with_runner(
                &parent,
                evaluation_request(None),
                |_, args, _| {
                    assert!(
                        !args.iter().any(|arg| arg == "control-policy"),
                        "no evaluator admission after an invalid index"
                    );
                    if args.iter().any(|arg| arg == "inspect") {
                        Ok(canonical_core_receipt())
                    } else if args.iter().any(|arg| arg == "held") {
                        Ok(json!({"items":[],"omitted":0}))
                    } else if args.iter().any(|arg| arg == "--full") {
                        Ok(full.clone())
                    } else {
                        Ok(show.clone())
                    }
                },
            )
            .err()
            .unwrap();
        let (status, message, calls) = match failure {
            "page_exhaustion" => (StatusCode::CONFLICT, "complete-index page bound", 16),
            "oversize_page" => (StatusCode::BAD_GATEWAY, "wire page bound", 1),
            "repeated_cursor" => (
                StatusCode::BAD_GATEWAY,
                "invalid or repeated continuation",
                2,
            ),
            _ => (
                StatusCode::BAD_GATEWAY,
                "invalid or repeated continuation",
                1,
            ),
        };
        assert_eq!(error.status, status, "{failure}: {error:?}");
        assert!(error.message.contains(message), "{failure}: {error:?}");
        assert_eq!(transport.requests.lock().unwrap().len(), calls);
        assert!(state.inner.lock().unwrap().delegations.is_empty());
    }
}
