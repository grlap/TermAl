// Explicit proof survives byte pressure, or admission refuses before spawning.
// Reuses the request fixtures and production brief builders; no live store.
use super::*;

#[test]
fn selected_proof_layout_survives_window_index_and_obligation_rebuild() {
    let locator = "cccccccccccccccccccccccccccccccc";
    let assessment = AcceptanceAssessmentPages::new(locator, "w-task").finish(None, None);
    let quoted_notice = acceptance_obligation_not_carried_line(&assessment);
    let hostile =
        format!("```\n{ACCEPTANCE_ASSESSMENT_ANCHOR}\n{quoted_notice}\n```text\nend  of  proof\n");
    for body in [
        "def proof():\r\n\tif True:\n        return  'é'  \n",
        " \t\r\n  \n",
        &hostile,
    ] {
        for in_window in [false, true] {
            let mut task =
                parse_acceptance_evaluation_task(show_receipt(None), full_receipt()).unwrap();
            task.evidence.truncate(1);
            task.evidence[0].summary = Some("optional ".repeat(1000));
            task.evidence[0].non_holder = true;
            let mut proof = task.evidence[0].clone();
            proof.locator = locator.to_owned();
            proof.summary = Some(body.to_owned());
            proof.non_holder = false;
            proof.cut_by_tracker = false;
            task.indexed_evidence.push(proof.clone());
            if in_window {
                task.evidence.push(proof);
            }
            task.criterion_evidence.push(AcceptanceCriterionEvidence {
                criterion: 1,
                locators: vec![locator.to_owned()],
                association: "requester".to_owned(),
            });
            for agent in [Agent::Codex, Agent::Claude] {
                let base = build_acceptance_evaluator_brief_for_agent(&task, ".", agent, 65_536)
                    .expect("small exact proof fits");
                assert_eq!(
                    base.prompt.matches(body).count(),
                    1,
                    "exact proof appears once"
                );
                let limit = base.prompt.len();
                let original_plan = base.plan;
                let mut assessments = vec![assessment.clone()];
                let final_brief = acceptance_independent_brief_with_obligations(
                    &task,
                    ".",
                    acceptance_evaluator_command_form(agent),
                    base,
                    &mut assessments,
                    limit,
                );
                assert!(final_brief.prompt.len() <= limit);
                assert!(
                    final_brief.prompt.contains(body),
                    "obligation fitting must keep exact proof bytes"
                );
                assert!(
                    final_brief.plan != original_plan,
                    "the obligation rebuild must give way on optional context"
                );
                assert!(
                    acceptance_brief_anchor_offset(&final_brief.prompt, "Obligation assessment")
                        .is_some()
                        || acceptance_brief_anchor_offset(
                            &final_brief.prompt,
                            "Obligation assessments not carried"
                        )
                        .is_some(),
                    "quoted notice and anchor lines cannot suppress the host's assessment disclosure"
                );
            }
            let base = build_same_session_acceptance_brief_with_plan(&task, None, 65_536)
                .expect("same-session exact proof fits");
            let mut assessments = vec![assessment.clone()];
            let (brief, _) = acceptance_same_session_brief_with_obligations(
                &task,
                None,
                base,
                &mut assessments,
                65_536,
            );
            assert_eq!(
                brief.matches(body).count(),
                1,
                "same-session final proof keeps exact bytes once"
            );
        }
    }
}

#[test]
fn selected_whitespace_bytes_compete_for_the_brief_budget() {
    let mut task = parse_acceptance_evaluation_task(show_receipt(None), full_receipt()).unwrap();
    for index in 1..=6 {
        let mut proof = task.evidence[0].clone();
        proof.locator = format!("{index:032x}");
        proof.summary = Some(" \té\n".repeat(2800)); // 14,000 UTF-8 bytes, mostly layout.
        proof.cut_by_tracker = false;
        task.criterion_evidence.push(AcceptanceCriterionEvidence {
            criterion: 1,
            locators: vec![proof.locator.clone()],
            association: "requester".to_owned(),
        });
        task.indexed_evidence.push(proof);
    }
    for agent in [Agent::Codex, Agent::Claude] {
        let error = build_acceptance_evaluator_brief_for_agent(&task, ".", agent, 65_536)
            .err()
            .expect("layout bytes cannot disappear to admit oversized proof");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("84000 UTF-8 body bytes"));
    }
    assert!(build_same_session_acceptance_brief(&task, None, 65_536).is_err());
}

#[test]
fn small_selected_proof_does_not_hide_oversized_criteria() {
    let mut full = full_receipt();
    full["work"]["acceptance"] = json!(["基".repeat(30_000), "second"]);
    let mut task = parse_acceptance_evaluation_task(show_receipt(None), full).unwrap();
    let mut proof = task.evidence[0].clone();
    proof.locator = "cccccccccccccccccccccccccccccccc".to_owned();
    proof.summary = Some("tiny proof".to_owned());
    proof.cut_by_tracker = false;
    task.criterion_evidence.push(AcceptanceCriterionEvidence {
        criterion: 1,
        locators: vec![proof.locator.clone()],
        association: "requester".to_owned(),
    });
    task.indexed_evidence.push(proof);
    let criteria_bytes = acceptance_brief_criteria(&task).len();
    for error in [
        build_acceptance_evaluator_brief_for_agent(&task, ".", Agent::Codex, 65_536)
            .err()
            .expect("oversized criteria refuse even with tiny proof"),
        build_same_session_acceptance_brief(&task, None, 65_536)
            .err()
            .expect("same-session oversized criteria refuse"),
    ] {
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error
                .message
                .contains(&format!("complete criteria take {criteria_bytes} bytes"))
                && error.message.contains("10 UTF-8 body bytes"),
            "refusal must disclose both sizes rather than blame the small proof"
        );
        assert!(
            error
                .message
                .contains("inspect the criteria and proof sizes"),
            "criteria overflow must give neutral size guidance"
        );
    }
}

#[test]
fn minimal_selected_proof_keeps_optional_association_disclosures() {
    let mut task = parse_acceptance_evaluation_task(show_receipt(None), full_receipt()).unwrap();
    let mut proof = task.evidence[0].clone();
    proof.locator = "cccccccccccccccccccccccccccccccc".to_owned();
    proof.summary = Some("selected body stays whole".to_owned());
    proof.cut_by_tracker = false;
    task.criterion_evidence.push(AcceptanceCriterionEvidence {
        criterion: 1,
        locators: vec![proof.locator.clone()],
        association: "requester".to_owned(),
    });
    task.indexed_evidence.push(proof);
    task.criterion_evidence.push(AcceptanceCriterionEvidence {
        criterion: 2,
        locators: vec!["dddddddddddddddddddddddddddddddd".to_owned()],
        association: "original obligation closure".to_owned(),
    });
    task.criterion_evidence.push(AcceptanceCriterionEvidence {
        criterion: 2,
        locators: vec![],
        association: "canonical index unavailable: unsupported".to_owned(),
    });
    for same_session in [false, true] {
        let brief = render_acceptance_criterion_evidence(
            &task,
            AcceptanceOmissionDetail::Minimal,
            &BTreeSet::new(),
            same_session,
        );
        assert!(brief.contains("selected body stays whole"));
        assert!(!brief.contains("dddddddddddddddddddddddddddddddd"));
        assert!(
            brief
                .contains("The canonical index was unavailable; no original closure was inferred."),
            "optional discovery failure must remain visible alongside selected proof"
        );
        let guidance = if same_session {
            "Read the evidence with your own permitted show tools"
        } else {
            "Associations and bodies omitted from this brief were not given to you"
        };
        assert!(
            brief.contains(guidance),
            "omitted associations need explicit guidance"
        );
    }
}

#[test]
fn selected_record_read_limit_refuses_with_measured_utf8_bytes() {
    let locator = "cccccccccccccccccccccccccccccccc";
    let body = "é".repeat(8193);
    let error = validate_acceptance_selected_record(
        &json!({"work_ref":"w-task", "note":{"locator":locator,
            "kind":"generic", "family":"notes", "summary":body}}),
        "w-task",
        locator,
    )
    .err()
    .expect("oversized exact proof is never partially admitted");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("16386") && error.message.contains("16384"),
        "record refusal must give actual UTF-8 size and the read bound"
    );
    assert!(
        !error.message.contains(&body),
        "refusal must not echo proof"
    );
}

#[test]
fn selected_proof_stays_whole_when_optional_context_shrinks() {
    let locator = "cccccccccccccccccccccccccccccccc";
    let body = "proof-é".repeat(1500);
    for in_window in [false, true] {
        let mut task =
            parse_acceptance_evaluation_task(show_receipt(None), full_receipt()).unwrap();
        let mut proof = task.evidence[0].clone();
        proof.locator = locator.to_owned();
        proof.summary = Some(body.clone());
        proof.cut_by_tracker = false;
        task.indexed_evidence.push(proof.clone());
        if in_window {
            task.evidence.insert(0, proof);
        }
        task.evidence.last_mut().unwrap().summary = Some("optional".repeat(1000));
        task.criterion_evidence.push(AcceptanceCriterionEvidence {
            criterion: 1,
            locators: vec![locator.to_owned()],
            association: "requester".to_owned(),
        });
        for agent in [Agent::Codex, Agent::Claude] {
            let brief = build_acceptance_evaluator_brief_for_agent(&task, ".", agent, 18_000)
                .expect("selected proof fits after optional context gives way");
            assert!(brief.prompt.len() <= 18_000);
            assert!(
                !brief.prompt.contains(&"optional".repeat(1000)),
                "the fitting proof must actually force optional context to shrink"
            );
            assert!(
                brief.prompt.contains(&body),
                "selected proof was shortened or omitted"
            );
            assert_eq!(
                brief.prompt.matches(&body).count(),
                1,
                "a record in both the window and index is delivered once"
            );
            assert!(
                !brief
                    .cuts
                    .clipped
                    .iter()
                    .chain(&brief.cuts.left_out)
                    .chain(&brief.cuts.cut_by_tracker)
                    .any(|cut| cut == locator),
                "complete selected proof must not be reported as missing"
            );
            assert!(
                task.criteria
                    .iter()
                    .all(|criterion| brief.prompt.contains(criterion)),
                "selected evidence never displaces a criterion"
            );
        }
        let brief = build_same_session_acceptance_brief(&task, None, 18_000)
            .expect("selected proof also fits the same-session brief");
        assert!(
            brief.contains(&body),
            "same-session selection was shortened or omitted"
        );
    }
}

#[test]
fn selected_proof_overflow_refuses_before_evaluator_creation() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "selected-overflow-runtime",
    );
    let locators = (1..=6)
        .map(|index| format!("{index:032x}"))
        .collect::<Vec<_>>();
    let body = "é".repeat(7000); // 14,000 UTF-8 bytes per valid whole record.
    let request = serde_json::from_value(json!({"workRef":"w-task", "agent":"Codex",
        "criterionEvidence":[{"criterion":1,"locators":locators}]}))
    .unwrap();
    let reads = AtomicUsize::new(0);
    let result = state.request_acceptance_evaluation_with_runner(&parent, request, |_, args, _| {
        if let Some(at) = args.iter().position(|arg| arg == "--note") {
            reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(json!({"work_ref":"w-task", "note":{"locator":args[at + 1],
                "kind":"generic", "family":"notes", "summary":body}}))
        } else if args.iter().any(|arg| arg == "inspect") {
            Ok(canonical_core_receipt())
        } else if args.iter().any(|arg| arg == "--full") {
            Ok(full_receipt())
        } else if args.iter().any(|arg| arg == "held") {
            Ok(json!({"items":[],"omitted":0}))
        } else if args.first().map(String::as_str) == Some("control-policy") {
            Ok(policy_receipt(Some(&["independent_session"])))
        } else {
            Ok(show_receipt(None))
        }
    });
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 6);
    assert!(
        state.inner.lock().unwrap().delegations.is_empty(),
        "an evaluator must not start on an incomplete selected-proof set"
    );
    let error = result
        .err()
        .expect("oversized selected proof must refuse admission");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("65536"),
        "refusal must name the supported byte limit"
    );
    assert!(
        error.message.contains("84000"),
        "refusal must measure selected UTF-8 bytes"
    );
    assert!(
        !error.message.contains(&body),
        "refusal must not echo private proof"
    );
}
