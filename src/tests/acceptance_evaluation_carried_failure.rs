// Tests for a carried failing evaluation in acceptance evaluation. Owns: the
// carried failure read from `show --full`, how both briefs show it as context
// that shrinks, the `--supersedes` the submission passes, and the refusal of a
// same-session evaluation that cannot acknowledge it. Does not own the
// tracker's own admission of the acknowledgement. Reuses the parent module's
// deterministic fixtures; no tracker process runs.
use super::*;

const FAILED: &str = "5bc7bccd9ef84c4f810696e190d65ce9";

/// `full_receipt` at revision 7 whose run carries `FAILED`, which judged
/// revision 2's criteria and failed criterion 1, revised since by `revised_by`.
fn carried_full_receipt(revised_by: &str, supersedes_required: bool) -> Value {
    let mut full = full_receipt();
    full["work"]["evaluation"] = json!({
        "hash": FAILED, "mode": "independent_session", "work_revision": 2,
        "carried_failure": {
            "evaluation": FAILED, "revised_by": revised_by, "judged_revision": 2,
            "failing": 1, "supersedes_required": supersedes_required,
            "judged_criteria": ["The old route exists everywhere", "A test covers it"],
            "blocking": [{"criterion": 1, "verdict": "fail",
                "rationale": "Only one of the routes exists."}],
            "judged_bindings": [{"criterion": 2, "requirement": {"check_kind": "test"}}]
        }
    });
    full
}

#[test]
fn the_carried_failure_is_read_from_the_full_contract_and_a_bad_id_is_dropped() {
    let task =
        parse_acceptance_evaluation_task(show_receipt(None), carried_full_receipt("executor", true))
            .unwrap();
    assert_eq!(
        task.carried_failure,
        Some(AcceptanceCarriedFailure {
            evaluation: FAILED.to_owned(),
            revised_by: "executor".to_owned(),
            judged_revision: 2,
            supersedes_required: true,
            judged_criteria: vec![
                "The old route exists everywhere".to_owned(),
                "A test covers it".to_owned()
            ],
            blocking: vec![(1, "fail".to_owned(), "Only one of the routes exists.".to_owned())],
            judged_bindings: vec![AcceptanceCriterionBinding {
                criterion: 2,
                check_kind: "test".to_owned(),
                fingerprint: None,
            }],
            newest: None,
        })
    );
    let mut full = carried_full_receipt("executor", true);
    full["work"]["evaluation"]["carried_failure"]["evaluation"] = json!("--supersedes-me");
    let task = parse_acceptance_evaluation_task(show_receipt(None), full).unwrap();
    assert_eq!(task.carried_failure, None);
    let task = parse_acceptance_evaluation_task(show_receipt(None), full_receipt()).unwrap();
    assert_eq!(task.carried_failure, None, "nothing carried, nothing read");
}

#[test]
fn both_briefs_show_the_carried_failure_and_it_shrinks_with_the_detail() {
    let task =
        parse_acceptance_evaluation_task(show_receipt(None), carried_full_receipt("planner", false))
            .unwrap();
    let header = format!("Carried failure: evaluation {FAILED} judged revision 2");
    let independent = build_acceptance_evaluator_brief(&task, "/repo", MAX_ACCEPTANCE_BRIEF_BYTES)
        .unwrap()
        .prompt;
    let same_session =
        build_same_session_acceptance_brief(&task, None, MAX_ACCEPTANCE_BRIEF_BYTES).unwrap();
    for (name, prompt) in [("independent", independent.as_str()), ("same_session", same_session.as_str())] {
        assert!(prompt.contains(&header), "{name}: {prompt}");
        assert!(prompt.contains("1. The old route exists everywhere"), "{name}");
        assert!(prompt.contains("fail: Only one of the routes exists."), "{name}");
        // The before side of the binding comparison, and the outcome question.
        assert!(prompt.contains("Bindings it judged: criterion 2 bound to `test`."), "{name}");
        assert!(prompt.contains("still deliver the task's outcome"), "{name}");
        // The current criteria stay the contract; the carried section follows them.
        assert!(prompt.find("1. The route exists").unwrap() < prompt.find(&header).unwrap(), "{name}");
    }
    // A planner's revision may be named by the session's own evaluation.
    assert!(same_session.contains(&format!(", supersedes {FAILED}, and exactly one verdict")));
    let compact = acceptance_brief_carried_failure(&task, AcceptanceOmissionDetail::Compact, false);
    assert!(compact.contains("1. fail: The old route exists everywhere"));
    assert!(!compact.contains("Only one of the routes exists."), "no rationale at compact detail");
    assert!(!compact.contains("2. A test covers it"), "only failing criteria at compact detail");
    assert!(compact.contains("Bindings it judged: criterion 2 bound to `test`."));
    let minimal = acceptance_brief_carried_failure(&task, AcceptanceOmissionDetail::Minimal, false);
    assert!(minimal.contains(&header));
    assert!(minimal.contains("not shown to fit the brief"));
    assert!(!minimal.contains("The old route"));
    // Minimal does not grow with the carried texts; compact grows only by the
    // clipped failing criteria.
    let mut long = task.clone();
    if let Some(carried) = long.carried_failure.as_mut() {
        carried.judged_criteria[0] = "x".repeat(5_000);
        carried.blocking[0].2 = "y".repeat(5_000);
    }
    assert_eq!(
        acceptance_brief_carried_failure(&long, AcceptanceOmissionDetail::Minimal, false),
        minimal
    );
    let long_compact = acceptance_brief_carried_failure(&long, AcceptanceOmissionDetail::Compact, false);
    assert!(long_compact.len() < compact.len() + 200, "the criterion is clipped");
    let long_full = acceptance_brief_carried_failure(&long, AcceptanceOmissionDetail::Full, false);
    assert!(long_full.len() < 2 * MAX_ACCEPTANCE_BRIEF_SUMMARY_CHARS + 1_000, "each text is clipped");
    assert!(minimal.len() < long_compact.len() && long_compact.len() < long_full.len());
    let none = parse_acceptance_evaluation_task(show_receipt(None), full_receipt()).unwrap();
    assert_eq!(acceptance_brief_carried_failure(&none, AcceptanceOmissionDetail::Full, false), "");
}

#[test]
fn the_request_persists_the_failure_to_acknowledge_and_same_session_is_refused_after_the_executors_revision() {
    for (mode, revised_by, required) in [
        ("independent_session", "executor", true),
        ("same_session", "executor", true),
        ("same_session", "planner", false),
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let full = carried_full_receipt(revised_by, required);
        let show = show_receipt(None);
        let reader = move |_: &EngramConnectionConfig, args: &[String], _: Duration| {
            if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&[mode])))
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items": [], "omitted": 0}))
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else {
                Ok(show.clone())
            }
        };
        let result = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            reader,
        );
        match (mode, required) {
            ("independent_session", _) => {
                let response = result.unwrap_or_else(|error| panic!("{}", error.message));
                let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
                    panic!("an independent evaluation spawns an evaluator");
                };
                let target = delegation.delegation.acceptance_evaluation.expect("target");
                assert_eq!(target.supersedes.as_deref(), Some(FAILED));
                // Persisted with the record, and absent on one stored before it.
                let stored = serde_json::to_value(&target).unwrap();
                assert_eq!(stored["supersedes"], FAILED);
                let mut legacy = stored.clone();
                legacy.as_object_mut().unwrap().remove("supersedes");
                let legacy: DelegationAcceptanceEvaluation = serde_json::from_value(legacy).unwrap();
                assert_eq!(legacy.supersedes, None);
            }
            ("same_session", true) => {
                let error = result.err().expect("a same-session acknowledgement is refused");
                assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
                assert!(error.message.contains(FAILED), "{}", error.message);
                assert!(error.message.contains("never held the run"), "{}", error.message);
                assert!(
                    error.message.contains("admit independent_session in the policy"),
                    "{}",
                    error.message
                );
            }
            _ => {
                let response = result.unwrap_or_else(|error| panic!("{}", error.message));
                let wire = serde_json::to_value(response).unwrap();
                assert!(wire["brief"].as_str().unwrap().contains(&format!(", supersedes {FAILED}")));
            }
        }
    }
}

fn connection() -> EngramConnectionConfig {
    EngramConnectionConfig {
        binary_path: PathBuf::from("engram"),
        project_file: PathBuf::from("/repo/.engram-project"),
        home: PathBuf::from("/home"),
        project_root: PathBuf::from("/repo"),
        actor_id: "greg/codex".to_owned(),
        actor_context: None,
        session_id: "session-child".to_owned(),
    }
}

#[test]
fn the_submission_names_the_carried_failure_and_refuses_a_stored_value_that_is_not_a_record_id() {
    let mut target = evaluation_target("delegation-a", 2);
    let request = two_verdicts();
    let args = acceptance_evaluation_cli_args(&connection(), &target, &request, None).unwrap();
    assert!(!args.iter().any(|arg| arg == "--supersedes"), "nothing carried, nothing named");
    target.supersedes = Some(FAILED.to_owned());
    let args = acceptance_evaluation_cli_args(&connection(), &target, &request, None).unwrap();
    let at = args.iter().position(|arg| arg == "--supersedes").expect("--supersedes is passed");
    assert_eq!(args[at + 1], FAILED);
    target.supersedes = Some("--json".to_owned());
    let refusal = acceptance_evaluation_cli_args(&connection(), &target, &request, None).unwrap_err();
    assert_eq!(refusal.status, StatusCode::CONFLICT);
    assert!(refusal.message.contains("not a record id"), "{}", refusal.message);
}

const NEWER: &str = "8026a47f2b89429c8c703741bc90689d";

#[test]
fn a_later_failing_evaluation_that_named_it_is_shown_as_the_middle_contract() {
    // The original failure stays carried; a later evaluation named it and
    // failed again on a contract that bound criterion 1 to a build, a binding
    // the executor then dropped. The next evaluator must see that middle side.
    let mut full = carried_full_receipt("executor", true);
    full["work"]["evaluation"]["hash"] = json!(NEWER);
    full["work"]["evaluation"]["verdicts"] = json!([
        {"position": 1, "criterion": "The route exists everywhere", "verdict": "fail",
         "basis": "judgment", "rationale": "Still only one route.", "citations": []},
        {"position": 2, "criterion": "A test covers it", "verdict": "pass",
         "basis": "observed", "rationale": "Covered.", "citations": []}
    ]);
    full["work"]["evaluation"]["carried_failure"]["newest_judged_bindings"] =
        json!([{"criterion": 1, "requirement": {"check_kind": "build"}}]);
    let task = parse_acceptance_evaluation_task(show_receipt(None), full).unwrap();
    let newest = task.carried_failure.as_ref().unwrap().newest.clone().expect("middle contract");
    assert_eq!(newest.evaluation, NEWER);
    assert_eq!(newest.blocking, vec![(1, "fail".to_owned(), "Still only one route.".to_owned())]);
    assert_eq!(newest.bindings[0].check_kind, "build");
    let brief = build_acceptance_evaluator_brief(&task, "/repo", MAX_ACCEPTANCE_BRIEF_BYTES)
        .unwrap()
        .prompt;
    assert!(brief.contains(&format!("A later evaluation, {NEWER}, named it and also did not pass 1")));
    assert!(brief.contains(&format!("Criteria evaluation {FAILED} judged:")));
    assert!(brief.contains(&format!("Criteria the later evaluation {NEWER} judged:")));
    assert!(brief.contains("fail: Still only one route."));
    assert!(brief.contains("Bindings it judged: criterion 1 bound to `build`."), "the dropped binding is visible");
    let compact = acceptance_brief_carried_failure(&task, AcceptanceOmissionDetail::Compact, false);
    assert!(compact.contains(&format!("Failing criteria of the later evaluation {NEWER}:")));
    assert!(compact.contains("Bindings it judged: criterion 1 bound to `build`."));
}

#[test]
fn the_carried_section_gives_way_before_evidence_and_the_outcome() {
    let mut task =
        parse_acceptance_evaluation_task(show_receipt(None), carried_full_receipt("executor", true))
            .unwrap();
    if let Some(carried) = task.carried_failure.as_mut() {
        carried.judged_criteria = (0..20).map(|index| format!("old criterion {index} {}", "c".repeat(700))).collect();
        carried.blocking = (1..=20).map(|position| (position, "fail".to_owned(), "r".repeat(700))).collect();
    }
    let full = build_acceptance_evaluator_brief(&task, "/repo", MAX_ACCEPTANCE_BRIEF_BYTES)
        .unwrap()
        .prompt;
    assert!(full.contains(&"r".repeat(100)), "rationales shown when everything fits");
    // One byte short: the carried rationales go before any evidence or outcome.
    let tighter = build_acceptance_evaluator_brief(&task, "/repo", full.len() - 1).unwrap().prompt;
    assert!(!tighter.contains(&"r".repeat(100)), "the carried section is compacted");
    assert!(tighter.contains("aaaaaaaa1111") && tighter.contains("bbbbbbbb2222"), "every evidence entry is kept");
    assert!(!tighter.contains("[clipped by the host"), "no evidence is clipped");
    assert!(tighter.contains("Outcome: The route answers."), "the outcome is whole");
}

#[test]
fn a_legacy_64_hex_id_is_kept_and_out_of_range_verdicts_are_dropped() {
    let legacy = "a".repeat(64);
    let mut full = carried_full_receipt("executor", true);
    full["work"]["evaluation"]["hash"] = json!(legacy);
    full["work"]["evaluation"]["carried_failure"]["evaluation"] = json!(legacy);
    full["work"]["evaluation"]["carried_failure"]["blocking"] = json!([
        {"criterion": 1, "verdict": "fail", "rationale": "kept"},
        {"criterion": 0, "verdict": "fail", "rationale": "no criterion 0"},
        {"criterion": 9, "verdict": "fail", "rationale": "past the end"}
    ]);
    let task = parse_acceptance_evaluation_task(show_receipt(None), full).unwrap();
    let carried = task.carried_failure.expect("a 64-hex record id is a record id");
    assert_eq!(carried.evaluation, legacy);
    assert_eq!(carried.blocking, vec![(1, "fail".to_owned(), "kept".to_owned())]);
    let mut target = evaluation_target("delegation-a", 2);
    target.supersedes = Some(legacy.clone());
    let args = acceptance_evaluation_cli_args(&connection(), &target, &two_verdicts(), None).unwrap();
    assert!(args.windows(2).any(|pair| pair[0] == "--supersedes" && pair[1] == legacy));
}

#[test]
fn the_acknowledgement_is_worded_for_the_mode_that_records_it() {
    let task =
        parse_acceptance_evaluation_task(show_receipt(None), carried_full_receipt("planner", false))
            .unwrap();
    let independent = acceptance_brief_carried_failure(&task, AcceptanceOmissionDetail::Full, false);
    assert!(independent.contains("the host names it to the tracker"));
    assert!(!independent.contains("when you record your evaluation"));
    let same_session = acceptance_brief_carried_failure(&task, AcceptanceOmissionDetail::Full, true);
    assert!(same_session.contains(&format!("name it as supersedes {FAILED} when you record your evaluation")));
    assert!(!same_session.contains("the host names it"));
}

#[test]
fn a_same_session_default_is_not_chosen_while_an_acknowledgement_is_required() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    state
        .update_acceptance_defaults(
            &project,
            AcceptanceEvaluatorDefaults {
                default_mode: Some(AcceptanceEvaluationMode::SameSession),
                ..Default::default()
            },
        )
        .unwrap();
    let reader = |pin: Option<&'static str>| {
        let full = carried_full_receipt("executor", true);
        let show = show_receipt(pin);
        move |_: &EngramConnectionConfig, args: &[String], _: Duration| {
            if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&["same_session", "independent_session"])))
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items": [], "omitted": 0}))
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else {
                Ok(show.clone())
            }
        }
    };
    // The default would pick same_session; the required acknowledgement moves
    // the choice to an evaluator that never held the run.
    let response = state
        .request_acceptance_evaluation_with_runner(&parent, evaluation_request(Some(Agent::Codex)), reader(None))
        .unwrap_or_else(|error| panic!("{}", error.message));
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(wire["mode"], "independent_session");
    // A pinned same_session cannot be moved; the refusal names the remedies.
    let error = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            reader(Some("same_session")),
        )
        .err()
        .expect("a pinned same_session cannot acknowledge the failure");
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert!(error.message.contains("unpin the task's evaluation mode"), "{}", error.message);
}

#[test]
fn the_carried_section_falls_to_minimal_before_any_evidence_is_clipped() {
    let mut task =
        parse_acceptance_evaluation_task(show_receipt(None), carried_full_receipt("executor", true))
            .unwrap();
    if let Some(carried) = task.carried_failure.as_mut() {
        carried.judged_criteria = (0..40).map(|index| format!("old criterion {index} {}", "c".repeat(700))).collect();
        carried.blocking = (1..=40).map(|position| (position, "fail".to_owned(), "r".repeat(700))).collect();
    }
    let compact = render_acceptance_evaluator_brief_with_details(
        &task,
        "/repo",
        task.evidence.len(),
        0,
        MAX_ACCEPTANCE_BRIEF_OUTCOME_BYTES,
        AcceptanceOmissionDetail::Full,
        AcceptanceOmissionDetail::Compact,
    )
    .prompt;
    // One byte short of the compact section: it falls to minimal while every
    // evidence entry stays whole and the outcome stays at its bound.
    let tighter = build_acceptance_evaluator_brief(&task, "/repo", compact.len() - 1).unwrap().prompt;
    assert!(tighter.contains("are not shown to fit the brief"), "the carried section is minimal");
    assert!(!tighter.contains("old criterion 0"));
    assert!(tighter.contains("aaaaaaaa1111") && tighter.contains("bbbbbbbb2222"));
    assert!(!tighter.contains("[clipped by the host"));
    assert!(tighter.contains("Outcome: The route answers."));
}

#[test]
fn a_persisted_target_without_supersedes_loads_and_newest_verdicts_are_keyed_by_position() {
    let mut stored = serde_json::to_value(evaluation_target("delegation-a", 2)).unwrap();
    stored.as_object_mut().unwrap().remove("supersedes");
    let persisted: PersistedDelegationAcceptanceEvaluation = serde_json::from_value(stored).unwrap();
    assert_eq!(DelegationAcceptanceEvaluation::from(persisted).supersedes, None);
    // Verdicts out of order, duplicated and with a gap: only the gapless run
    // from position 1 is kept, each verdict under its own criterion.
    let mut full = carried_full_receipt("executor", true);
    full["work"]["evaluation"]["hash"] = json!(NEWER);
    full["work"]["evaluation"]["verdicts"] = json!([
        {"position": 2, "criterion": "second", "verdict": "fail", "rationale": "second failed"},
        {"position": 1, "criterion": "first", "verdict": "pass", "rationale": "ok"},
        {"position": 2, "criterion": "duplicate", "verdict": "fail", "rationale": "dropped"},
        {"position": 4, "criterion": "after a gap", "verdict": "fail", "rationale": "dropped"}
    ]);
    let task = parse_acceptance_evaluation_task(show_receipt(None), full).unwrap();
    let newest = task.carried_failure.unwrap().newest.unwrap();
    assert_eq!(newest.criteria, vec!["first".to_owned(), "second".to_owned()]);
    assert_eq!(newest.blocking, vec![(2, "fail".to_owned(), "second failed".to_owned())]);
}
