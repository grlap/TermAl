// Obligation assessments of verification records in the acceptance evaluator
// brief and the evaluation request result.
//
// Owns: tests that a verification record the brief carries brings a compact,
// status-grouped summary of its obligation assessment, read whole through the
// record's assessment pages, with every row another record did not close in
// full and the closed rows only counted; that an assessment that cannot be
// read whole says how much was read and where to resume; and that a summary is
// carried whole or named as not carried, never taking room from the brief.
// Does not own: the general criterion-evidence selection tests, which stay in
// acceptance_evaluation_evidence.rs; this module is declared from
// acceptance_evaluation.rs and reuses its fixtures. No live tracker is touched.

use super::*;

/// The obligation assessment of one real verification record, captured page by
/// page from the tracker's `show --note --json` and its `--after`
/// continuations: 7 pages, 49 rows, 47 of them left out as already closed by
/// another record and 2 satisfied by this record (15512 bytes, sha256
/// f682c795f5a521d2f68fdfa2db38d5ff246011e9b5e733d6e963a11310586ebf).
const CAPTURED_ASSESSMENT_PAGES: &str = include_str!("fixtures/obligation-assessment-pages.json");

const RECORD: &str = "95400d92b3fc4737918ba329534b0ceb";

fn captured_assessment_pages() -> Vec<Value> {
    serde_json::from_str(CAPTURED_ASSESSMENT_PAGES).expect("the captured pages are JSON")
}

/// The `--after` token of a page's continuation command, if it has one.
fn continuation_token(page: &Value) -> Option<String> {
    let command = page.get("continuation")?.as_str()?;
    let mut words = command.split_whitespace();
    words.find(|word| *word == "--after")?;
    words.next().map(str::to_owned)
}

fn verification_note(locator: &str, assessment: Option<&Value>) -> Value {
    let mut note = json!({
        "locator": locator, "kind": "verification", "family": "notes",
        "by": "peer-holder (agent=claude)", "created_at": "2026-10-05T01:18:16Z",
        "summary": "`node scripts/test-launcher.mjs full` exited 0\nfull gate passed: 5 stages",
        "body_bytes": 617, "non_holder": false,
        "verification": {"check_kind": "test", "producer_outcome": "succeeded",
            "result": "passed", "source_revision": "content-v1:e053bbdf"}
    });
    if let Some(assessment) = assessment {
        note["assessment"] = assessment.clone();
    }
    note
}

/// Serves `pages` as the assessment of `RECORD`: the exact record read carries
/// the first page, and each `--after` read the page that follows the one whose
/// continuation named that token. A read of page `fail_from` or later fails.
fn assessment_reply(
    pages: &[Value],
    fail_from: Option<usize>,
    args: &[String],
) -> Option<Result<Value, EngramTransportError>> {
    let note = args.iter().position(|arg| arg == "--note")?;
    assert_eq!(args[note + 1], RECORD, "only the cited record is read");
    Some(match args.iter().position(|arg| arg == "--after") {
        None => Ok(json!({"work_ref": "w-task", "note": verification_note(RECORD, pages.first())})),
        Some(after) => {
            let token = &args[after + 1];
            let index = pages
                .iter()
                .position(|page| continuation_token(page).as_deref() == Some(token.as_str()))
                .expect("a continuation token the tracker issued")
                + 1;
            if fail_from.is_some_and(|fail| index >= fail) {
                Err(EngramTransportError::transport("the tracker process exited"))
            } else {
                Ok(json!({"work_ref": "w-task", "locator": RECORD, "assessment": pages[index]}))
            }
        }
    })
}

/// Requests an independent evaluation whose criterion 1 cites `RECORD`, and
/// returns the request result and the evaluator child's prompt.
fn evaluate_with_assessment(pages: Vec<Value>, fail_from: Option<usize>) -> (Value, String) {
    let (wire, prompt, _) = evaluate_counting_note_reads(pages, fail_from);
    (wire, prompt)
}

/// [`evaluate_with_assessment`], also counting the `--note` reads of `RECORD`.
fn evaluate_counting_note_reads(
    pages: Vec<Value>,
    fail_from: Option<usize>,
) -> (Value, String, usize) {
    let note_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = note_reads.clone();
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "obligation-assessment-runtime",
    );
    let request = serde_json::from_value(json!({"workRef": "w-task", "agent": "Codex",
        "criterionEvidence": [{"criterion": 1, "locators": [RECORD]}]}))
    .unwrap();
    let show = show_receipt(None);
    let response = state
        .request_acceptance_evaluation_with_runner(&parent, request, move |_, args, _| {
            if let Some(reply) = assessment_reply(&pages, fail_from, args) {
                if !args.iter().any(|arg| arg == "--after") {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                reply
            } else if args.iter().any(|arg| arg == "inspect") {
                Ok(super::evidence_selection::canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full_receipt())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items": [], "omitted": 0}))
            } else if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&["independent_session"])))
            } else {
                Ok(show.clone())
            }
        })
        .expect("the evaluation request is admitted");
    let wire = serde_json::to_value(response).unwrap();
    let inner = state.inner.lock().unwrap();
    let prompt = inner
        .delegations
        .iter()
        .find(|row| row.id == wire["delegation"]["id"].as_str().unwrap())
        .expect("the evaluator child")
        .prompt
        .clone();
    let reads = note_reads.load(std::sync::atomic::Ordering::SeqCst);
    (wire, prompt, reads)
}

/// The assessment rows rendered in full in a brief: one indented line each,
/// naming the row's status, rule and trigger position.
fn rendered_rows(prompt: &str) -> Vec<&str> {
    prompt
        .lines()
        .filter(|line| line.trim_start().starts_with("- obligation "))
        .collect()
}

fn full_history_command(locator: &str) -> String {
    format!("engram work show w-task --note {locator} --json")
}

#[test]
fn a_cited_verification_record_brings_its_assessment_grouped_by_status_with_the_rows_that_matter_in_full() {
    let (wire, prompt) = evaluate_with_assessment(captured_assessment_pages(), None);

    let header = format!(
        "Obligation assessment of {RECORD} (record position 787, cut position 788): 49 rows, read whole."
    );
    assert!(prompt.contains(&header), "{prompt}");
    assert!(
        prompt.contains("47 left_out (already_closed; satisfied_by_another_record)"),
        "{prompt}"
    );
    assert!(prompt.contains("2 matches (satisfied_by_this_record)"), "{prompt}");
    assert_eq!(
        rendered_rows(&prompt),
        vec![
            "  - obligation matches: acceptance_criterion_requires_verification:6 v1, criterion 6, check test, recorded satisfied_by_this_record, trigger 768",
            "  - obligation matches: source_mutation_requires_test v1, check test, recorded satisfied_by_this_record, trigger 778",
        ],
        "only the rows this record satisfies are inlined: {prompt}"
    );

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["locator"], json!(RECORD), "{wire}");
    assert_eq!(summary["total"], json!(49));
    assert_eq!(summary["rowsRead"], json!(49));
    assert_eq!(summary["complete"], json!(true));
    assert_eq!(summary["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        compact_acceptance_evaluation_request_result(&wire)["obligationAssessments"],
        wire["obligationAssessments"]
    );
}

#[test]
fn a_mismatch_on_the_last_assessment_page_is_carried_in_full() {
    let mut pages = captured_assessment_pages();
    let last = pages.last_mut().unwrap();
    // A failed check this record could not close: the obligation stays open.
    last["rows"][0] = json!({"check_kind": "test", "mismatch": "result_not_passed",
        "pinned": false, "recorded": "open",
        "rule": "source_mutation_requires_test", "rule_version": 1,
        "status": "mismatch", "trigger_position": 778});

    let (wire, prompt) = evaluate_with_assessment(pages, None);

    assert!(
        prompt.contains("1 mismatch (result_not_passed; open)"),
        "{prompt}"
    );
    assert!(
        rendered_rows(&prompt).contains(
            &"  - obligation mismatch (result_not_passed): source_mutation_requires_test v1, check test, recorded open, trigger 778"
        ),
        "{prompt}"
    );
    assert_eq!(rendered_rows(&prompt).len(), 2, "{prompt}");
    assert_eq!(
        wire["obligationAssessments"][0]["rows"][1]["status"],
        json!("mismatch"),
        "{wire}"
    );
}

/// A stale failed check, whose obligation another record closed later, is
/// still a mismatch a reader must see: it is listed in full, not counted.
#[test]
fn a_mismatch_whose_obligation_another_record_closed_is_still_listed_in_full() {
    let mut pages = captured_assessment_pages();
    let last = pages.last_mut().unwrap();
    last["rows"][0] = json!({"check_kind": "test", "mismatch": "result_not_passed",
        "pinned": false, "recorded": "satisfied_by_another_record",
        "rule": "source_mutation_requires_test", "rule_version": 1,
        "status": "mismatch", "trigger_position": 778});

    let (wire, prompt) = evaluate_with_assessment(pages, None);

    assert!(
        prompt.contains("1 mismatch (result_not_passed; satisfied_by_another_record)"),
        "{prompt}"
    );
    assert!(
        rendered_rows(&prompt).contains(
            &"  - obligation mismatch (result_not_passed): source_mutation_requires_test v1, check test, recorded satisfied_by_another_record, trigger 778"
        ),
        "{prompt}"
    );
    assert_eq!(rendered_rows(&prompt).len(), 2, "{prompt}");
    assert_eq!(
        wire["obligationAssessments"][0]["rows"][1]["status"],
        json!("mismatch"),
        "{wire}"
    );
}

#[test]
fn the_summary_names_its_cut_and_the_full_history_command_and_inlines_no_closed_row() {
    let pages = captured_assessment_pages();
    let first = pages[0]["continuation"].as_str().unwrap().to_owned();
    let (wire, prompt) = evaluate_with_assessment(pages, None);

    assert!(prompt.contains("(record position 787, cut position 788)"), "{prompt}");
    assert!(
        prompt.contains(&format!(
            "  Full history at this cut, oldest first: page 1 by `{}`, then `{first}`.",
            full_history_command(RECORD)
        )),
        "{prompt}"
    );
    assert!(!prompt.contains("Resume at the same cut"), "{prompt}");
    assert!(
        !rendered_rows(&prompt).iter().any(|row| row.contains("left_out")),
        "{prompt}"
    );
    assert_eq!(
        wire["obligationAssessments"][0]["fullHistory"],
        json!(full_history_command(RECORD))
    );
    let section = prompt.find("Obligation assessments of the cited verification records").unwrap();
    assert!(
        prompt.find("Criterion evidence index").unwrap() < section
            && section < prompt.find("Evidence recorded on the task").unwrap(),
        "the section sits between the criterion index and the evidence list: {prompt}"
    );
}

#[test]
fn an_assessment_cut_by_a_failed_read_says_how_much_was_read_and_where_to_resume() {
    let pages = captured_assessment_pages();
    let resume = pages[1]["continuation"].as_str().unwrap().to_owned();

    let (wire, prompt) = evaluate_with_assessment(pages, Some(2));

    assert!(
        prompt.contains(&format!(
            "Obligation assessment of {RECORD} (record position 787, cut position 788): INCOMPLETE: 16 of 49 rows read (transport_failure)."
        )),
        "{prompt}"
    );
    assert!(prompt.contains("an unread row may be open or a mismatch"), "{prompt}");
    assert!(
        prompt.contains("16 left_out (already_closed; satisfied_by_another_record)"),
        "{prompt}"
    );
    assert!(prompt.contains(&format!("  Resume at the same cut: `{resume}`")), "{prompt}");
    assert!(rendered_rows(&prompt).is_empty(), "{prompt}");
    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["complete"], json!(false));
    assert_eq!(summary["rowsRead"], json!(16));
    assert_eq!(summary["total"], json!(49));
    assert_eq!(summary["incompleteReason"], json!("transport_failure"));
    assert_eq!(summary["resume"], json!(resume));
    let notice = wire["notice"].as_str().unwrap();
    assert!(
        notice.contains(&format!(
            "{RECORD}: incomplete, 16 of 49 rows read (transport_failure); resume at the same cut with `{resume}`"
        )),
        "{notice}"
    );
}

#[test]
fn an_assessment_whose_continuation_is_missing_is_incomplete_never_presented_whole() {
    let mut pages = captured_assessment_pages();
    pages[2].as_object_mut().unwrap().remove("continuation");
    let fetched_third = pages[1]["continuation"].as_str().unwrap().to_owned();

    let (wire, prompt) = evaluate_with_assessment(pages, None);

    assert!(
        prompt.contains("INCOMPLETE: 24 of 49 rows read (missing_continuation)."),
        "{prompt}"
    );
    assert!(!prompt.contains("read whole"), "{prompt}");
    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["complete"], json!(false));
    assert_eq!(summary["rowsRead"], json!(24));
    assert_eq!(summary["resume"], json!(fetched_third), "{wire}");
    let notice = wire["notice"].as_str().unwrap();
    assert!(
        notice.contains(&format!(
            "{RECORD}: incomplete, 24 of 49 rows read (missing_continuation); resume at the same cut with `{fetched_third}`; it re-reads page 3 (rows 17-24)"
        )),
        "{notice}"
    );
}

#[test]
fn a_verification_record_without_an_assessment_leaves_the_brief_unchanged() {
    let (wire, prompt) = evaluate_with_assessment(Vec::new(), None);

    assert!(prompt.contains(&format!("Criterion 1: {RECORD} (requester)")), "{prompt}");
    assert!(!prompt.contains("Obligation assessment"), "{prompt}");
    assert_eq!(wire["obligationAssessments"], json!([]));
    assert!(
        !wire["notice"]
            .as_str()
            .unwrap_or_default()
            .contains("Obligation assessment"),
        "{wire}"
    );
}

#[test]
fn an_original_closure_record_brings_its_assessment_into_the_same_session_brief() {
    const CLOSURE: &str = "cccccccccccccccccccccccccccccccc";
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, page) = super::evidence_selection::canonical_binding_receipts();
    let _transport =
        super::evidence_selection::install_binding_evidence_transport(&state, &parent, vec![page]);
    let assessment = json!({"total": 3, "earlier": 0, "shown": 3, "omitted": 0,
        "record_position": 33, "cut_position": 34, "label": "reconstructed at record position 33",
        "rows": [
            {"check_kind": "test", "criterion": 1, "left_out": "not_yet_defined", "pinned": false,
                "recorded": "open",
                "rule": "acceptance_criterion_requires_verification:1", "rule_version": 1,
                "status": "left_out", "trigger_position": 3},
            {"check_kind": "test", "criterion": 2, "pinned": false,
                "recorded": "satisfied_by_this_record",
                "rule": "acceptance_criterion_requires_verification:2", "rule_version": 1,
                "status": "matches", "trigger_position": 3},
            {"check_kind": "test", "left_out": "already_closed", "pinned": false,
                "recorded": "satisfied_by_another_record",
                "rule": "source_mutation_requires_test", "rule_version": 1,
                "status": "left_out", "trigger_position": 20}
        ]});
    let response = state
        .request_acceptance_evaluation_with_runner(&parent, evaluation_request(None), |_, args, _| {
            if let Some(note) = args.iter().position(|arg| arg == "--note") {
                assert_eq!(args[note + 1], CLOSURE, "the closure record is read exactly");
                Ok(json!({"work_ref": "w-task", "note": verification_note(CLOSURE, Some(&assessment))}))
            } else if args.iter().any(|arg| arg == "inspect") {
                Ok(super::evidence_selection::canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
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
        brief.contains(&format!("Criterion 2: {CLOSURE} (original obligation closure)")),
        "{brief}"
    );
    assert!(
        brief.contains(&format!(
            "Obligation assessment of {CLOSURE} (record position 33, cut position 34): 3 rows, read whole. 1 left_out (not_yet_defined; open), 1 matches (satisfied_by_this_record), 1 left_out (already_closed; satisfied_by_another_record)."
        )),
        "{brief}"
    );
    assert_eq!(
        rendered_rows(brief),
        vec![
            "  - obligation left_out (not_yet_defined): acceptance_criterion_requires_verification:1 v1, criterion 1, check test, recorded open, trigger 3",
            "  - obligation matches: acceptance_criterion_requires_verification:2 v1, criterion 2, check test, recorded satisfied_by_this_record, trigger 3",
        ],
        "{brief}"
    );
    assert!(
        brief.find("Obligation assessments of the cited verification records").unwrap()
            < brief.find("Evidence not shown in this brief:").unwrap(),
        "{brief}"
    );
    assert_eq!(wire["obligationAssessments"][0]["carried"], json!(true));
}

fn reader_task() -> AcceptanceEvaluationTask {
    let mut task = parse_acceptance_evaluation_task(show_receipt(None), full_receipt())
        .expect("the fixture task parses");
    task.indexed_evidence.push(AcceptanceEvaluationEvidence {
        locator: RECORD.to_owned(),
        kind: "verification".to_owned(),
        by: None,
        created_at: None,
        summary: Some("full gate passed".to_owned()),
        non_holder: false,
        body_bytes: None,
        cut_by_tracker: false,
        verification: None,
    });
    task
}

fn reader_connection() -> EngramConnectionConfig {
    EngramConnectionConfig {
        binary_path: PathBuf::from("engram"),
        project_file: PathBuf::from("/repo/.engram-project"),
        home: PathBuf::from("/home"),
        project_root: PathBuf::from("/repo"),
        actor_id: "greg/claude".to_owned(),
        actor_context: None,
        session_id: "session-holder".to_owned(),
    }
}

fn reader_show_args() -> Vec<String> {
    ["work", "show", "w-task", "--notes", "--gates", "--json"].map(str::to_owned).to_vec()
}

/// `count` one-row pages of left-out rows, chained by continuations.
fn one_row_pages(count: u64) -> Vec<Value> {
    (0..count)
        .map(|index| {
            let mut page = json!({"total": count, "earlier": index, "shown": 1,
                "omitted": count - 1, "record_position": 10, "cut_position": 11,
                "rows": [{"check_kind": "test", "left_out": "already_closed", "pinned": false,
                    "recorded": "satisfied_by_another_record",
                    "rule": "source_mutation_requires_test", "rule_version": 1,
                    "status": "left_out", "trigger_position": index}]});
            if index + 1 < count {
                page["continuation"] = json!(format!(
                    "engram work show w-task --note {RECORD} --after token-{index}"
                ));
            }
            page
        })
        .collect()
}

/// Runs the assessment reader over `task` with `reply` answering every read.
fn read_records(
    mut task: AcceptanceEvaluationTask,
    reply: impl Fn(&[String]) -> Result<Value, EngramTransportError>,
    deadline: std::time::Instant,
) -> Vec<AcceptanceObligationAssessment> {
    read_acceptance_obligation_assessments(
        &mut task,
        &reader_connection(),
        &reader_show_args(),
        &|_, args: &[String], _| reply(args),
        deadline,
        &std::time::Instant::now,
    );
    task.obligation_assessments
}

fn read_with(
    pages: &[Value],
    read_after: impl Fn(usize) -> Result<Value, EngramTransportError>,
    deadline: std::time::Instant,
) -> AcceptanceObligationAssessment {
    let mut assessments = read_records(
        reader_task(),
        |args| match args.iter().position(|arg| arg == "--after") {
            None => Ok(json!({"work_ref": "w-task", "note": verification_note(RECORD, pages.first())})),
            Some(after) => {
                let index = args[after + 1]
                    .trim_start_matches("token-")
                    .parse::<usize>()
                    .unwrap();
                read_after(index + 1)
            }
        },
        deadline,
    );
    assert_eq!(assessments.len(), 1);
    assessments.remove(0)
}

fn later() -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(60)
}

#[test]
fn reading_stops_at_the_page_bound_and_names_where_to_resume() {
    let pages = one_row_pages(40);
    let served = pages.clone();
    let assessment = read_with(
        &pages,
        |index| Ok(json!({"work_ref": "w-task", "locator": RECORD, "assessment": served[index]})),
        later(),
    );

    assert!(!assessment.complete);
    assert_eq!(assessment.incomplete_reason.as_deref(), Some("page_limit"));
    assert_eq!(assessment.rows_read, 32);
    assert_eq!(assessment.total, Some(40));
    assert_eq!(
        assessment.resume.as_deref(),
        pages[31]["continuation"].as_str()
    );
}

#[test]
fn a_frame_bound_or_spent_budget_is_named_as_the_reason_the_read_stopped() {
    let pages = one_row_pages(3);
    let framed = read_with(
        &pages,
        |_| {
            Err(EngramTransportError::transport(
                "Engram show output exceeds the maximum control frame",
            ))
        },
        later(),
    );
    assert_eq!(framed.incomplete_reason.as_deref(), Some("frame_bound"));
    assert_eq!(framed.rows_read, 1);
    assert_eq!(framed.resume.as_deref(), pages[0]["continuation"].as_str());

    let spent = read_with(&pages, |_| unreachable!("no budget is left"), std::time::Instant::now());
    assert_eq!(spent.incomplete_reason.as_deref(), Some("time_budget"));
    assert_eq!(spent.total, None);
    assert!(render_acceptance_obligation_assessment(&spent)
        .contains("INCOMPLETE: 0 of an unknown number of rows read (time_budget)."));
}

#[test]
fn a_summary_without_room_is_named_or_left_out_and_never_displaces_the_brief() {
    let mut pages = AcceptanceAssessmentPages::new(RECORD, "w-task");
    for page in captured_assessment_pages() {
        pages.add(&page).expect("the captured pages chain");
    }
    let assessment = pages.finish(None, None);
    assert!(assessment.complete);
    let whole = render_acceptance_obligation_assessment(&assessment);
    let short = acceptance_obligation_not_carried_line(&assessment);
    let prompt = format!(
        "{}\n{ACCEPTANCE_ASSESSMENT_ANCHOR} — cite by locator:\n  - a cited record\n",
        "Criteria, the criterion evidence index and a fail-first note. ".repeat(40)
    );
    let at = prompt.find(ACCEPTANCE_ASSESSMENT_ANCHOR).unwrap();
    let (mut saw_whole, mut saw_short, mut saw_none) = (false, false, false);
    for room in (0..whole.len() + 400).step_by(5) {
        let mut assessments = vec![assessment.clone()];
        let max_bytes = prompt.len() + room;
        let fitted = acceptance_prompt_with_obligations(
            prompt.clone(),
            ACCEPTANCE_ASSESSMENT_ANCHOR,
            &mut assessments,
            max_bytes,
        );
        assert!(fitted.len() <= max_bytes, "room {room}");
        let section_len = fitted.len() - prompt.len();
        assert_eq!(
            format!("{}{}", &fitted[..at], &fitted[at + section_len..]),
            prompt,
            "the brief around the section is unchanged at room {room}"
        );
        let carried = fitted.contains(&whole);
        assert_eq!(assessments[0].carried, carried, "room {room}");
        if carried {
            saw_whole = true;
        } else if fitted.contains(&short) {
            saw_short = true;
        } else {
            assert_eq!(fitted, prompt, "room {room}");
            saw_none = true;
        }
    }
    assert!(saw_whole && saw_short && saw_none);

    let mut not_carried = assessment.clone();
    not_carried.carried = false;
    let notice = acceptance_obligation_assessment_notice(&[not_carried]).unwrap();
    let first = captured_assessment_pages()[0]["continuation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        notice.contains(&format!(
            "{RECORD}: read whole (49 rows) but not carried to fit the brief; the full history at this cut, oldest first, is page 1 by `{}` and then `{first}`",
            full_history_command(RECORD)
        )),
        "{notice}"
    );
}

// Round 2: the review pair's findings, each tested against the first freeze.

fn verification_entry(locator: &str) -> AcceptanceEvaluationEvidence {
    AcceptanceEvaluationEvidence {
        locator: locator.to_owned(),
        kind: "verification".to_owned(),
        by: None,
        created_at: None,
        summary: Some("full gate passed".to_owned()),
        non_holder: false,
        body_bytes: None,
        cut_by_tracker: false,
        verification: None,
    }
}

fn captured_assessment() -> AcceptanceObligationAssessment {
    let mut pages = AcceptanceAssessmentPages::new(RECORD, "w-task");
    for page in captured_assessment_pages() {
        pages.add(&page).expect("the captured pages chain");
    }
    pages.finish(None, None)
}

/// A verification record already whole in the newest-notes window that is
/// also an original closure is not added to the index, yet the brief cites it:
/// its assessment must be read too.
#[test]
fn an_original_closure_already_whole_in_the_notes_window_still_brings_its_assessment() {
    const CLOSURE: &str = "cccccccccccccccccccccccccccccccc";
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (mut show, full, page) = super::evidence_selection::canonical_binding_receipts();
    show["notes"].as_array_mut().unwrap().insert(0, json!({
        "locator": CLOSURE, "kind": "verification", "family": "notes",
        "summary": "The original bound test passed", "body_bytes": 30, "non_holder": false,
        "verification": {"check_kind": "test", "result": "passed", "source_revision": "git:older"}}));
    let _transport =
        super::evidence_selection::install_binding_evidence_transport(&state, &parent, vec![page]);
    let assessment = json!({"total": 1, "earlier": 0, "shown": 1, "omitted": 0,
        "record_position": 33, "cut_position": 34,
        "rows": [{"check_kind": "test", "criterion": 2, "pinned": false,
            "recorded": "satisfied_by_this_record",
            "rule": "acceptance_criterion_requires_verification:2", "rule_version": 1,
            "status": "matches", "trigger_position": 3}]});
    let response = state
        .request_acceptance_evaluation_with_runner(&parent, evaluation_request(None), |_, args, _| {
            if let Some(note) = args.iter().position(|arg| arg == "--note") {
                assert_eq!(args[note + 1], CLOSURE, "the closure record is read exactly");
                Ok(json!({"work_ref": "w-task", "note": verification_note(CLOSURE, Some(&assessment))}))
            } else if args.iter().any(|arg| arg == "inspect") {
                Ok(super::evidence_selection::canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
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
        brief.contains(&format!("Criterion 2: {CLOSURE} (original obligation closure)")),
        "{brief}"
    );
    assert_eq!(wire["obligationAssessments"][0]["locator"], json!(CLOSURE), "{wire}");
    assert!(
        brief.contains(&format!(
            "Obligation assessment of {CLOSURE} (record position 33, cut position 34): 1 rows, read whole."
        )),
        "{brief}"
    );
}

/// The one-line note for a summary the brief has no room for keeps what an
/// incomplete read says: rows read of the total, the reason and where to resume.
#[test]
fn the_not_carried_line_of_an_incomplete_assessment_keeps_its_state_and_resume_point() {
    let captured = captured_assessment_pages();
    let resume = captured[1]["continuation"].as_str().unwrap().to_owned();
    let mut pages = AcceptanceAssessmentPages::new(RECORD, "w-task");
    for page in &captured[..2] {
        pages.add(page).expect("the captured pages chain");
    }
    let incomplete = pages.finish(Some("transport_failure"), Some(resume.clone()));

    let line = acceptance_obligation_not_carried_line(&incomplete);

    assert!(line.contains("INCOMPLETE"), "{line}");
    assert!(line.contains("16 of 49 rows read (transport_failure)"), "{line}");
    assert!(line.contains(&resume), "{line}");
    assert!(!line.contains("49 rows, 0 listed in full"), "{line}");
}

/// A later page the host rejects still leaves the tracker's continuation that
/// fetched it, so the reader resumes there rather than from the start.
#[test]
fn a_rejected_later_page_keeps_the_continuation_that_fetched_it() {
    let pages = one_row_pages(3);
    let mut rejected = pages[1].clone();
    rejected["rows"][0]["rule"] = json!("r".repeat(MAX_ACCEPTANCE_ASSESSMENT_FIELD_CHARS + 1));
    let assessment = read_with(
        &pages,
        |_| Ok(json!({"work_ref": "w-task", "locator": RECORD, "assessment": rejected})),
        later(),
    );

    assert_eq!(assessment.incomplete_reason.as_deref(), Some("malformed_row"));
    assert_eq!(assessment.rows_read, 1);
    assert_eq!(assessment.resume.as_deref(), pages[0]["continuation"].as_str());
}

/// An exact read for a closure record must return that record, a holder
/// verification note: another note, another kind or no note at all is an
/// invalid receipt, never an assessment and never "no assessment".
#[test]
fn an_exact_read_that_returns_another_note_or_none_is_an_invalid_receipt() {
    let pages = one_row_pages(1);
    let other = "dddddddddddddddddddddddddddddddd";
    let mut generic = verification_note(RECORD, pages.first());
    generic["kind"] = json!("generic");
    for receipt in [
        json!({"work_ref": "w-task", "note": verification_note(other, pages.first())}),
        json!({"work_ref": "w-task", "note": generic}),
        json!({"work_ref": "w-task"}),
    ] {
        let assessments = read_records(reader_task(), |_| Ok(receipt.clone()), later());
        assert_eq!(assessments.len(), 1, "{receipt}");
        assert!(!assessments[0].complete, "{receipt}");
        assert_eq!(
            assessments[0].incomplete_reason.as_deref(),
            Some("invalid_receipt"),
            "{receipt}"
        );
    }
}

/// `left_out` means skipped before matching; whether the obligation is
/// closed is its `recorded` end. A left-out row still open is listed in full.
#[test]
fn a_left_out_row_whose_recorded_end_is_open_is_listed_in_full() {
    let page = json!({"total": 3, "earlier": 0, "shown": 3, "omitted": 0,
        "record_position": 20, "cut_position": 21, "rows": [
            {"check_kind": "test", "left_out": "already_closed", "pinned": false,
                "recorded": "satisfied_by_another_record",
                "rule": "source_mutation_requires_test", "rule_version": 1,
                "status": "left_out", "trigger_position": 4},
            {"check_kind": "test", "criterion": 3, "left_out": "not_yet_defined", "pinned": false,
                "recorded": "open",
                "rule": "acceptance_criterion_requires_verification:3", "rule_version": 1,
                "status": "left_out", "trigger_position": 9},
            {"check_kind": "test", "pinned": false, "recorded": "satisfied_by_this_record",
                "rule": "source_mutation_requires_test", "rule_version": 1,
                "status": "matches", "trigger_position": 12}]});
    let mut pages = AcceptanceAssessmentPages::new(RECORD, "w-task");
    pages.add(&page).expect("one whole page");
    let assessment = pages.finish(None, None);

    let rendered = render_acceptance_obligation_assessment(&assessment);
    assert!(
        rendered.contains(
            "  - obligation left_out (not_yet_defined): acceptance_criterion_requires_verification:3 v1, criterion 3, check test, recorded open, trigger 9"
        ),
        "{rendered}"
    );
    assert_eq!(rendered_rows(&rendered).len(), 2, "{rendered}");
    assert!(
        rendered.contains("1 left_out (not_yet_defined; open)")
            && rendered.contains("1 left_out (already_closed; satisfied_by_another_record)"),
        "{rendered}"
    );
}

/// A selected verification record whose exact read carried no assessment is
/// not read a second time to learn the same absence.
#[test]
fn a_selected_record_without_an_assessment_is_read_once() {
    let (_, prompt, note_reads) = evaluate_counting_note_reads(Vec::new(), None);

    assert!(!prompt.contains("Obligation assessment"), "{prompt}");
    assert_eq!(note_reads, 1);
}

/// The section goes before the anchor line, never at an earlier occurrence of
/// the anchor phrase inside quoted record text.
#[test]
fn the_section_is_placed_at_the_anchor_line_not_inside_quoted_text() {
    let mut assessments = vec![captured_assessment()];
    let prompt = format!(
        "Outcome: the note says {ACCEPTANCE_ASSESSMENT_ANCHOR} - quoted.\nCriteria.\n{ACCEPTANCE_ASSESSMENT_ANCHOR} - cite by locator:\n  - entry\n"
    );
    let fitted = acceptance_prompt_with_obligations(
        prompt.clone(),
        ACCEPTANCE_ASSESSMENT_ANCHOR,
        &mut assessments,
        prompt.len() + 4096,
    );

    let quoted = fitted.find("- quoted.").unwrap();
    let section = fitted.find("Obligation assessments of the cited").unwrap();
    assert!(quoted < section, "{fitted}");
    assert!(
        fitted[section..].contains(&format!("\n{ACCEPTANCE_ASSESSMENT_ANCHOR} - cite by locator:")),
        "{fitted}"
    );
}

/// A continuation that is not one line of plain text is not quoted as the
/// resume point; the read stops and points to the full history instead.
#[test]
fn a_continuation_with_control_characters_is_never_quoted() {
    let mut pages = one_row_pages(2);
    pages[0]["continuation"] = json!(format!(
        "engram work show w-task --note {RECORD}\n--after token-0"
    ));
    let served = pages.clone();
    let assessment = read_with(
        &pages,
        |index| Ok(json!({"work_ref": "w-task", "locator": RECORD, "assessment": served[index]})),
        later(),
    );

    assert!(!assessment.complete);
    assert_eq!(assessment.incomplete_reason.as_deref(), Some("missing_continuation"));
    assert_eq!(assessment.resume, None);
}

/// A page that does not continue the rows read (a gap, another cut or another
/// total) stops the read as a changed assessment, keeping where it stopped.
#[test]
fn a_page_that_does_not_continue_the_rows_read_stops_as_a_changed_assessment() {
    let pages = one_row_pages(3);
    for (field, value) in [("earlier", json!(5)), ("cut_position", json!(99)), ("total", json!(7))] {
        let mut changed = pages[1].clone();
        changed[field] = value;
        let assessment = read_with(
            &pages,
            |_| Ok(json!({"work_ref": "w-task", "locator": RECORD, "assessment": changed})),
            later(),
        );
        assert_eq!(assessment.incomplete_reason.as_deref(), Some("assessment_changed"), "{field}");
        assert_eq!(assessment.rows_read, 1, "{field}");
        assert_eq!(assessment.resume.as_deref(), pages[0]["continuation"].as_str(), "{field}");
    }
}

/// A continuation receipt for another record, or a continuation naming
/// another record, is not followed.
#[test]
fn a_continuation_for_another_record_is_not_followed() {
    let pages = one_row_pages(2);
    let served = pages.clone();
    let foreign = read_with(
        &pages,
        |index| {
            Ok(json!({"work_ref": "w-task", "locator": "dddddddddddddddddddddddddddddddd",
                "assessment": served[index]}))
        },
        later(),
    );
    assert_eq!(foreign.incomplete_reason.as_deref(), Some("invalid_receipt"));

    let mut naming = one_row_pages(2);
    naming[0]["continuation"] =
        json!("engram work show w-task --note dddddddddddddddddddddddddddddddd --after token-0");
    let named = read_with(&naming, |_| unreachable!("a foreign continuation is not followed"), later());
    assert_eq!(named.incomplete_reason.as_deref(), Some("missing_continuation"));
    assert_eq!(named.resume, None);
}

/// The per-request page budget is shared by every record: once it is spent,
/// the next record says so rather than being read or skipped silently.
#[test]
fn the_request_page_budget_is_shared_across_records() {
    let locators = ["a", "b", "c", "d"].map(|letter| letter.repeat(32));
    let mut task = reader_task();
    task.indexed_evidence = locators.iter().map(|locator| verification_entry(locator)).collect();
    let page_of = |locator: &str, index: u64| {
        json!({"total": 40, "earlier": index, "shown": 1, "omitted": 39,
            "record_position": 10, "cut_position": 11,
            "rows": [{"check_kind": "test", "left_out": "already_closed", "pinned": false,
                "recorded": "satisfied_by_another_record",
                "rule": "source_mutation_requires_test", "rule_version": 1,
                "status": "left_out", "trigger_position": index}],
            "continuation": format!("engram work show w-task --note {locator} --after {locator}-{index}")})
    };
    let assessments = read_records(
        task,
        |args| {
            let note = args.iter().position(|arg| arg == "--note").unwrap();
            let locator = args[note + 1].clone();
            match args.iter().position(|arg| arg == "--after") {
                None => Ok(json!({"work_ref": "w-task",
                    "note": verification_note(&locator, Some(&page_of(&locator, 0)))})),
                Some(after) => {
                    let index = args[after + 1].rsplit('-').next().unwrap().parse::<u64>().unwrap();
                    Ok(json!({"work_ref": "w-task", "locator": locator,
                        "assessment": page_of(&locator, index + 1)}))
                }
            }
        },
        later(),
    );

    let read = assessments.iter().map(|a| a.rows_read).collect::<Vec<_>>();
    assert_eq!(read, vec![32, 32, 32, 0]);
    assert!(assessments
        .iter()
        .all(|a| a.incomplete_reason.as_deref() == Some("page_limit")));
}

/// Two summaries fit in index order: the first whole, the second as its
/// one-line note when only that fits.
#[test]
fn two_summaries_fit_in_index_order_whole_then_named() {
    let first = captured_assessment();
    let mut second = first.clone();
    second.locator = "dddddddddddddddddddddddddddddddd".to_owned();
    second.full_history = format!("engram work show w-task --note {} --json", second.locator);
    let whole = render_acceptance_obligation_assessment(&first);
    let short = acceptance_obligation_not_carried_line(&second);
    let prompt = format!("Criteria.\n{ACCEPTANCE_ASSESSMENT_ANCHOR} - cite by locator:\n");
    let room = 200 + whole.len() + short.len();
    let mut assessments = vec![first, second];
    let fitted = acceptance_prompt_with_obligations(
        prompt.clone(),
        ACCEPTANCE_ASSESSMENT_ANCHOR,
        &mut assessments,
        prompt.len() + room,
    );

    assert!(assessments[0].carried && !assessments[1].carried);
    assert!(fitted.find(&whole).unwrap() < fitted.find(&short).unwrap(), "{fitted}");
}

// Round 3: the assessment deadline, a malformed selected assessment and the
// section header.

/// Requests an independent evaluation whose criterion 1 cites `RECORD` on a
/// host clock the tracker reads advance: the exact selected read leaves the
/// clock `selected_at` seconds after the start, and the first `--after` read
/// leaves it `first_after_at` seconds after the start. Returns the request
/// result and how many `--after` reads were made.
fn evaluate_on_clock(selected_at: u64, first_after_at: Option<u64>) -> (Value, usize) {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    let pages = captured_assessment_pages();
    let elapsed = Arc::new(AtomicU64::new(0));
    let after_reads = Arc::new(AtomicUsize::new(0));
    let (clock, counted) = (elapsed.clone(), after_reads.clone());
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "obligation-assessment-clock-runtime",
    );
    let request = serde_json::from_value(json!({"workRef": "w-task", "agent": "Codex",
        "criterionEvidence": [{"criterion": 1, "locators": [RECORD]}]}))
    .unwrap();
    let show = show_receipt(None);
    let start = std::time::Instant::now();
    let response = state
        .request_acceptance_evaluation_until(
            &parent,
            request,
            move |_, args, _| {
                if let Some(reply) = assessment_reply(&pages, None, args) {
                    if args.iter().any(|arg| arg == "--after") {
                        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                            if let Some(at) = first_after_at {
                                clock.store(at, Ordering::SeqCst);
                            }
                        }
                    } else {
                        clock.store(selected_at, Ordering::SeqCst);
                    }
                    reply
                } else if args.iter().any(|arg| arg == "inspect") {
                    Ok(super::evidence_selection::canonical_core_receipt())
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full_receipt())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items": [], "omitted": 0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["independent_session"])))
                } else {
                    Ok(show.clone())
                }
            },
            start + Duration::from_secs(600),
            move || start + Duration::from_secs(elapsed.load(Ordering::SeqCst)),
        )
        .expect("the evaluation request is admitted, its closing reads funded");
    let reads = after_reads.load(Ordering::SeqCst);
    (serde_json::to_value(response).unwrap(), reads)
}

/// The discovery deadline already holds back the closing reserve. Earlier
/// reads that spend 25 of its 40 seconds must still leave the assessment the
/// rest; the reserve is not taken twice.
#[test]
fn assessment_reads_use_the_rest_of_the_discovery_window() {
    let (wire, after_reads) = evaluate_on_clock(25, None);

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["complete"], json!(true), "{wire}");
    assert_eq!(summary["rowsRead"], json!(49), "{wire}");
    assert_eq!(after_reads, 6);
}

/// Assessment reads stop at the discovery deadline, so the closing basis and
/// identity reads keep the whole closing reserve: a read that leaves the clock
/// past that deadline is the last one, and the request still completes.
#[test]
fn assessment_reads_stop_at_the_discovery_deadline_and_leave_the_closing_reserve() {
    let (wire, after_reads) = evaluate_on_clock(25, Some(41));

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(after_reads, 1, "no assessment read after the discovery deadline: {wire}");
    assert_eq!(summary["complete"], json!(false), "{wire}");
    assert_eq!(summary["incompleteReason"], json!("time_budget"), "{wire}");
    assert_eq!(summary["rowsRead"], json!(16), "{wire}");
}

/// A selected record whose assessment is present but not an object was not
/// read whole: it is an incomplete summary, never taken for no assessment.
#[test]
fn a_selected_record_with_a_malformed_assessment_is_incomplete_not_absent() {
    let (wire, prompt, note_reads) = evaluate_counting_note_reads(vec![json!([1, 2])], None);

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["locator"], json!(RECORD), "{wire}");
    assert_eq!(summary["complete"], json!(false), "{wire}");
    assert_eq!(summary["incompleteReason"], json!("malformed_page"), "{wire}");
    assert!(
        prompt.contains("INCOMPLETE: 0 of an unknown number of rows read (malformed_page)."),
        "{prompt}"
    );
    assert_eq!(note_reads, 1, "the exact read is not repeated");
}

/// The section header says what the listing does: closed rows are counted,
/// except mismatches, which are always listed.
#[test]
fn the_section_header_names_the_mismatch_exception() {
    let mut assessments = vec![captured_assessment()];
    let prompt = format!("Criteria.\n{ACCEPTANCE_ASSESSMENT_ANCHOR} - cite by locator:\n");
    let fitted = acceptance_prompt_with_obligations(
        prompt.clone(),
        ACCEPTANCE_ASSESSMENT_ANCHOR,
        &mut assessments,
        prompt.len() + 4096,
    );

    let header = fitted
        .lines()
        .find(|line| line.starts_with("Obligation assessments of the cited"))
        .expect("the section header");
    assert!(header.contains("except mismatches, which are always listed"), "{header}");
}

// Round 5: page metadata and a continuation after the last row.

/// A page's count, offset and positions are what make a read whole at one
/// cut. Each must be a present non-negative integer, and the rows an array;
/// a page that does not say so is malformed, never read as whole.
#[test]
fn page_metadata_that_is_not_a_non_negative_integer_is_a_malformed_page() {
    let page = one_row_pages(1).remove(0);
    let mut failures = Vec::new();
    for (field, value) in [
        ("total", json!("1")),
        ("total", json!(-1)),
        ("earlier", json!("0")),
        ("earlier", json!(-1)),
        ("record_position", json!("10")),
        ("record_position", json!(-10)),
        ("cut_position", json!({"position": 11})),
        ("cut_position", json!(1.5)),
        ("rows", json!({"0": {}})),
    ] {
        let mut malformed = page.clone();
        malformed[field] = value.clone();
        let mut pages = AcceptanceAssessmentPages::new(RECORD, "w-task");
        if pages.add(&malformed) != Err("malformed_page") {
            failures.push(format!("{field} = {value}"));
        }
    }
    for field in ["total", "earlier", "record_position", "cut_position", "rows"] {
        let mut missing = page.clone();
        missing.as_object_mut().unwrap().remove(field);
        let mut pages = AcceptanceAssessmentPages::new(RECORD, "w-task");
        if pages.add(&missing) != Err("malformed_page") {
            failures.push(format!("{field} missing"));
        }
    }
    assert!(failures.is_empty(), "accepted as a page: {failures:?}");
}

/// A continuation page with a negative offset is malformed, and the read
/// resumes at the continuation that fetched it.
#[test]
fn a_continuation_page_with_malformed_metadata_is_a_malformed_page() {
    let pages = one_row_pages(3);
    let mut malformed = pages[1].clone();
    malformed["earlier"] = json!(-1);
    let assessment = read_with(
        &pages,
        |_| Ok(json!({"work_ref": "w-task", "locator": RECORD, "assessment": malformed})),
        later(),
    );

    assert_eq!(assessment.incomplete_reason.as_deref(), Some("malformed_page"));
    assert_eq!(assessment.rows_read, 1);
    assert_eq!(assessment.resume.as_deref(), pages[0]["continuation"].as_str());
}

/// Once every row is read, the read is whole: a continuation the last page
/// still carries is not followed, so its failure cannot demote the read.
#[test]
fn a_whole_read_is_complete_even_when_its_last_page_carries_a_continuation() {
    let mut pages = one_row_pages(3);
    pages[2]["continuation"] =
        json!(format!("engram work show w-task --note {RECORD} --after token-2"));
    let served = pages.clone();
    let assessment = read_with(
        &pages,
        |index| match served.get(index) {
            Some(page) => Ok(json!({"work_ref": "w-task", "locator": RECORD, "assessment": page})),
            None => Err(EngramTransportError::transport("the tracker process exited")),
        },
        later(),
    );

    assert!(assessment.complete, "{assessment:?}");
    assert_eq!(assessment.rows_read, 3);
    assert_eq!(assessment.incomplete_reason, None);
}

// Evaluation 1: the same-cut continuation. The tracker's cursors bind the
// captured cut; a cursorless `show --note` reads the current state, so it is
// never offered as a continuation at the cut.

/// The first page's own continuation is what pages the rest of the history
/// at the captured cut: each summary keeps and names it.
#[test]
fn each_summary_names_the_first_page_continuation_for_the_history_at_its_cut() {
    let pages = captured_assessment_pages();
    let first = pages[0]["continuation"].as_str().unwrap().to_owned();

    let (wire, prompt) = evaluate_with_assessment(pages, None);

    assert_eq!(
        wire["obligationAssessments"][0]["historyContinuation"],
        json!(first),
        "{wire}"
    );
    let history = prompt
        .lines()
        .find(|line| line.trim_start().starts_with("Full history"))
        .expect("the full-history line");
    assert!(history.contains(&first), "{history}");
}

/// A whole summary the brief has no room for still names the same-cut
/// continuation in its one-line note.
#[test]
fn the_not_carried_line_of_a_whole_read_names_the_same_cut_continuation() {
    let first = captured_assessment_pages()[0]["continuation"]
        .as_str()
        .unwrap()
        .to_owned();

    let line = acceptance_obligation_not_carried_line(&captured_assessment());

    assert!(line.contains(&first), "{line}");
}

/// A later page that names no next page before the read is whole leaves the
/// cursor that fetched it: resuming there re-reads that page at the same cut,
/// and the summary and notice say so with its rows.
#[test]
fn a_missing_continuation_mid_read_resumes_at_the_cursor_that_fetched_the_last_page() {
    let mut pages = captured_assessment_pages();
    pages[2].as_object_mut().unwrap().remove("continuation");
    let fetched_third = pages[1]["continuation"].as_str().unwrap().to_owned();

    let (wire, prompt) = evaluate_with_assessment(pages, None);

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["incompleteReason"], json!("missing_continuation"), "{wire}");
    assert_eq!(summary["resume"], json!(fetched_third), "{wire}");
    assert_eq!(
        summary["resumeRereads"],
        json!({"page": 3, "firstRow": 17, "lastRow": 24}),
        "{wire}"
    );
    assert!(prompt.contains("re-reads page 3 (rows 17-24)"), "{prompt}");
    let notice = wire["notice"].as_str().unwrap();
    assert!(notice.contains("re-reads page 3 (rows 17-24)"), "{notice}");
}

/// The same holds when the next continuation cannot be quoted: the cursor
/// that fetched the last page read is offered, never nothing.
#[test]
fn an_unquotable_continuation_mid_read_resumes_at_the_cursor_that_fetched_the_last_page() {
    let mut pages = one_row_pages(3);
    pages[1]["continuation"] = json!(format!(
        "engram work show w-task --note {RECORD}\n--after token-1"
    ));
    let served = pages.clone();
    let assessment = read_with(
        &pages,
        |index| Ok(json!({"work_ref": "w-task", "locator": RECORD, "assessment": served[index]})),
        later(),
    );
    let wire = serde_json::to_value(&assessment).unwrap();

    assert_eq!(wire["incompleteReason"], json!("missing_continuation"), "{wire}");
    assert_eq!(wire["resume"], pages[0]["continuation"], "{wire}");
    assert_eq!(wire["resumeRereads"], json!({"page": 2, "firstRow": 2, "lastRow": 2}), "{wire}");
}

/// A read that stops on its first page has no same-cut cursor at all: the
/// summary says so and never offers the cursorless command as one.
#[test]
fn a_first_page_stop_says_no_same_cut_continuation_exists() {
    let mut pages = captured_assessment_pages();
    pages[0].as_object_mut().unwrap().remove("continuation");

    let (wire, prompt) = evaluate_with_assessment(pages, None);

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["incompleteReason"], json!("missing_continuation"), "{wire}");
    assert!(summary.get("historyContinuation").is_none(), "{wire}");
    assert!(summary.get("resume").is_none(), "{wire}");
    assert!(prompt.contains("No same-cut continuation exists"), "{prompt}");
    assert!(!prompt.contains("Resume at the same cut"), "{prompt}");
    let notice = wire["notice"].as_str().unwrap();
    assert!(notice.contains("No same-cut continuation exists"), "{notice}");
}

// Evaluation 2: a brief with no room even for the one-line note. The brief
// names the record unless naming it would displace protected content, which
// is only the criterion-cited records and notes.

/// How many bytes a test leaves free under the brief bound: fewer than any
/// line naming a not-carried record.
const SATURATED_SLACK: usize = 30;

/// An independent evaluation citing `RECORD` whose first criterion is padded
/// by `criterion_fill` characters, with `window` as the newest-notes window.
fn evaluate_filled(pages: Vec<Value>, window: Value, criterion_fill: usize) -> (Value, String) {
    evaluate_filled_in(pages, window, criterion_fill, false)
}

/// [`evaluate_filled`] in either mode; the same-session brief is the
/// response's own brief.
fn evaluate_filled_in(
    pages: Vec<Value>,
    window: Value,
    criterion_fill: usize,
    same_session: bool,
) -> (Value, String) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "obligation-assessment-fill-runtime",
    );
    let request = serde_json::from_value(json!({"workRef": "w-task", "agent": "Codex",
        "criterionEvidence": [{"criterion": 1, "locators": [RECORD]}]}))
    .unwrap();
    let mode = if same_session { "same_session" } else { "independent_session" };
    let criterion = format!("The route exists {}", "x".repeat(criterion_fill));
    let mut show = show_receipt(None);
    show["notes"] = window;
    show["status"]["work"]["acceptance"][0] = json!(criterion);
    let mut full = full_receipt();
    full["work"]["acceptance"][0] = json!(criterion);
    let response = state
        .request_acceptance_evaluation_with_runner(&parent, request, move |_, args, _| {
            if let Some(reply) = assessment_reply(&pages, None, args) {
                reply
            } else if args.iter().any(|arg| arg == "inspect") {
                Ok(super::evidence_selection::canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items": [], "omitted": 0}))
            } else if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&[mode])))
            } else {
                Ok(show.clone())
            }
        })
        .expect("the evaluation request is admitted");
    let wire = serde_json::to_value(response).unwrap();
    if same_session {
        let brief = wire["brief"].as_str().expect("the same-session brief").to_owned();
        return (wire, brief);
    }
    let inner = state.inner.lock().unwrap();
    let prompt = inner
        .delegations
        .iter()
        .find(|row| row.id == wire["delegation"]["id"].as_str().unwrap())
        .expect("the evaluator child")
        .prompt
        .clone();
    (wire, prompt)
}

/// The same evaluation with the criterion padded so that the finished brief
/// leaves exactly `SATURATED_SLACK` bytes free: measured once without an
/// assessment, whose brief is otherwise the same.
fn evaluate_saturated(window: Value) -> (Value, String) {
    evaluate_saturated_in(window, false)
}

fn evaluate_saturated_in(window: Value, same_session: bool) -> (Value, String) {
    let (_, measured) = evaluate_filled_in(Vec::new(), window.clone(), 0, same_session);
    assert!(measured.len() + SATURATED_SLACK < MAX_ACCEPTANCE_BRIEF_BYTES);
    let fill = MAX_ACCEPTANCE_BRIEF_BYTES - measured.len() - SATURATED_SLACK;
    evaluate_filled_in(captured_assessment_pages(), window, fill, same_session)
}

/// The brief names `RECORD` as not carried: by its one-line note, or on the
/// line that names records whose note does not fit either.
fn names_record_as_not_carried(prompt: &str) -> bool {
    prompt.contains(&format!("Obligation assessment of {RECORD}: not carried to fit the brief"))
        || prompt.contains(&format!(
            "Obligation assessments not carried in this brief (the requester notice has them): {RECORD}."
        ))
}

/// An uncited non-holder note in the window: the only evidence a rebuilt
/// brief may clip or leave out to name a not-carried record.
fn uncited_window_note(locator: &str) -> Value {
    uncited_window_note_of(locator, 2000, false)
}

fn uncited_window_note_of(locator: &str, chars: usize, holder: bool) -> Value {
    json!({"locator": locator, "kind": "note", "family": "notes", "body_bytes": chars,
        "by": "peer-observer (agent=codex)", "created_at": "2026-09-18T10:00:00Z",
        "non_holder": !holder,
        "summary": format!("An uncited progress note. {}", "y".repeat(chars))})
}

/// When clipping uncited evidence frees room for the line naming the record
/// but not for its one-line note, that line names it.
#[test]
fn a_saturated_brief_names_the_record_on_the_names_line_when_its_note_does_not_fit() {
    let window = json!([
        uncited_window_note_of("aaaaaaaa1111", 900, false),
        uncited_window_note_of("bbbbbbbb2222", 900, false),
        uncited_window_note_of("cccccccc3333", 900, false),
    ]);

    let (wire, prompt) = evaluate_saturated(window);

    assert!(prompt.len() <= MAX_ACCEPTANCE_BRIEF_BYTES);
    assert!(
        prompt.contains(&format!(
            "Obligation assessments not carried in this brief (the requester notice has them): {RECORD}."
        )),
        "len={} omissions={}",
        prompt.len(),
        wire["evidenceOmissions"]
    );
    assert!(prompt.contains("Criterion evidence index"), "the index keeps its detail");
    assert!(prompt.contains("full gate passed: 5 stages"), "the cited body stays whole");
}

/// The brief names `RECORD` as not carried in neither form.
fn names_record_nowhere(prompt: &str) -> bool {
    !prompt.contains("Obligation assessment")
}

/// An uncited holder note may be the holder's fail-first note, which the
/// brief cannot tell from any other: it is protected, so a brief that could
/// make room only by clipping it stays as it is, the note byte for byte, and
/// the notice alone names the record.
#[test]
fn an_uncited_holder_note_never_gives_way_and_the_notice_alone_names_the_record() {
    let holder_note = uncited_window_note_of("aaaaaaaa1111", 2000, true);
    let body = holder_note["summary"].as_str().unwrap().to_owned();

    let (wire, prompt) = evaluate_saturated(json!([holder_note]));

    assert!(prompt.contains(&body), "the holder note is carried whole");
    assert!(names_record_nowhere(&prompt), "{}", &prompt[prompt.len().saturating_sub(1500)..]);
    assert_eq!(wire["evidenceOmissions"]["clipped"]["count"], json!(0), "{wire}");
    assert_eq!(wire["evidenceOmissions"]["leftOut"]["count"], json!(0), "{wire}");
    let notice = wire["notice"].as_str().unwrap();
    assert!(notice.contains(&format!("{RECORD}: read whole (49 rows) but not carried")), "{notice}");
}

/// A holder note the brief already had to clip is still protected: a rebuild
/// may not leave it out to make room for naming the record, so its clipped
/// line stays byte for byte and the notice alone names the record.
#[test]
fn an_already_clipped_holder_note_is_never_left_out_to_name_the_record() {
    const HOLDER: &str = "aaaaaaaa1111";
    let window = json!([
        uncited_window_note_of(HOLDER, 2000, true),
        uncited_window_note_of("bbbbbbbb2222", 20, false),
        uncited_window_note_of("cccccccc3333", 20, false),
    ]);
    // A probe that overflows by more than the index can give back makes the
    // builder clip the holder note; the brief grows by the criterion fill
    // alone, so the second fill leaves `SATURATED_SLACK` bytes with that clip.
    let (_, unclipped) = evaluate_filled(Vec::new(), window.clone(), 0);
    let probe_fill = MAX_ACCEPTANCE_BRIEF_BYTES - unclipped.len() + 1000;
    let (probe_wire, probe) = evaluate_filled(Vec::new(), window.clone(), probe_fill);
    assert_eq!(probe_wire["evidenceOmissions"]["clipped"]["locators"], json!([HOLDER]), "{probe_wire}");
    let clipped_line = probe
        .lines()
        .find(|line| line.contains("[clipped by the host:") &&line.contains(HOLDER))
        .expect("the probe lists the holder note clipped")
        .to_owned();
    let fill = probe_fill + (MAX_ACCEPTANCE_BRIEF_BYTES - SATURATED_SLACK) - probe.len();

    let (wire, prompt) = evaluate_filled(captured_assessment_pages(), window, fill);

    assert!(prompt.len() <= MAX_ACCEPTANCE_BRIEF_BYTES);
    assert!(
        prompt.contains(&clipped_line),
        "the clipped holder note stays byte for byte; omissions={} base omissions={}",
        wire["evidenceOmissions"],
        probe_wire["evidenceOmissions"]
    );
    assert_eq!(
        wire["evidenceOmissions"], probe_wire["evidenceOmissions"],
        "the brief gives way on nothing more than the probe did"
    );
    assert_eq!(wire["obligationAssessments"][0]["carried"], json!(false), "{wire}");
    let notice = wire["notice"].as_str().unwrap();
    assert!(notice.contains(&format!("{RECORD}: read whole (49 rows) but not carried")), "{notice}");
}

fn window_holder_note(index: usize) -> Value {
    json!({"locator": format!("dddd{index:08}"), "kind": "note", "family": "notes",
        "body_bytes": 20, "by": "greg/claude", "created_at": "2026-09-18T10:00:00Z",
        "non_holder": false, "summary": "A short progress note."})
}

fn window_holder_verification(index: usize) -> Value {
    json!({"locator": format!("{:032x}", 0xabc0 + index), "kind": "verification",
        "family": "notes", "body_bytes": 300, "by": "greg/claude",
        "created_at": "2026-09-18T10:00:00Z", "non_holder": false,
        "summary": format!("An earlier check. {}", "z".repeat(300)),
        "verification": {"check_kind": "test", "result": "passed", "source_revision": "git:older"}})
}

/// A same-session brief carries no window bodies, only the omitted locators;
/// it makes room by lowering its omission detail (the locator names give
/// way), keeping the criterion-cited bodies in its index and the earlier
/// checks it lists.
#[test]
fn a_saturated_same_session_brief_names_a_not_carried_record_by_dropping_omitted_locator_names() {
    let window = Value::Array((0..30).map(window_holder_note).collect());

    let (wire, brief) = evaluate_saturated_in(window, true);

    assert!(brief.len() <= MAX_ACCEPTANCE_BRIEF_BYTES);
    assert!(names_record_as_not_carried(&brief), "len={}", brief.len());
    assert!(brief.contains(&format!("Criterion 1: {RECORD} (requester)")));
    assert!(brief.contains("full gate passed: 5 stages"), "the cited body stays whole");
    assert_eq!(wire["obligationAssessments"][0]["carried"], json!(false), "{wire}");
}

/// The same-session brief's omission detail also selects how many earlier
/// checks it lists. They are holder records it carries, so they never give
/// way: the brief stays as it is and the notice alone names the record.
#[test]
fn a_saturated_same_session_brief_keeps_its_listed_checks_and_the_notice_alone_names_the_record() {
    let window = Value::Array((0..10).map(window_holder_verification).collect());

    let (wire, brief) = evaluate_saturated_in(window, true);

    assert!(names_record_nowhere(&brief), "{}", &brief[brief.len().saturating_sub(1500)..]);
    for index in 0..10 {
        let line = format!("  - {:032x} (verification test passed at git:older)", 0xabc0 + index);
        assert!(brief.contains(&line), "every listed check stays: {line}");
    }
    let notice = wire["notice"].as_str().unwrap();
    assert!(notice.contains(&format!("{RECORD}: read whole (49 rows) but not carried")), "{notice}");
}

/// A brief saturated by content that is not protected (uncited notes in the
/// window) gives way on that content so that it can name the record whose
/// assessment it does not carry; every criterion-cited body stays whole.
#[test]
fn a_saturated_brief_names_a_not_carried_record_when_only_unprotected_content_gives_way() {
    let window = json!([
        uncited_window_note("aaaaaaaa1111"),
        uncited_window_note("bbbbbbbb2222"),
        uncited_window_note("cccccccc3333"),
    ]);

    let (wire, prompt) = evaluate_saturated(window);

    assert!(prompt.len() <= MAX_ACCEPTANCE_BRIEF_BYTES);
    assert!(
        names_record_as_not_carried(&prompt),
        "len={} omissions={}",
        prompt.len(),
        wire["evidenceOmissions"]
    );
    assert_eq!(
        wire["evidenceOmissions"]["clipped"]["locators"],
        json!(["aaaaaaaa1111"]),
        "only the oldest uncited window note gave way"
    );
    assert!(prompt.contains("Criterion evidence index"), "the index keeps its detail");
    assert!(prompt.contains(&format!("Criterion 1: {RECORD} (requester)")), "the citation stays");
    assert!(
        prompt.contains("full gate passed: 5 stages"),
        "the cited record's body stays whole"
    );
    assert_eq!(wire["obligationAssessments"][0]["carried"], json!(false), "{wire}");
    let notice = wire["notice"].as_str().unwrap();
    assert!(notice.contains(&format!("{RECORD}: read whole (49 rows) but not carried")), "{notice}");
}

/// A brief saturated by protected content alone (the criteria and the
/// criterion-cited record, with no other evidence to give way) is left exactly
/// as it would be without the assessment: the requester notice alone names
/// the record.
#[test]
fn a_brief_saturated_by_protected_content_is_unchanged_and_the_notice_alone_names_the_record() {
    let (_, without) = evaluate_filled(Vec::new(), json!([]), 0);
    let fill = MAX_ACCEPTANCE_BRIEF_BYTES - without.len() - SATURATED_SLACK;
    let (_, measured) = evaluate_filled(Vec::new(), json!([]), fill);

    let (wire, prompt) = evaluate_saturated(json!([]));

    assert_eq!(prompt.len(), measured.len(), "the brief is not changed by the assessment");
    assert!(!prompt.contains("Obligation assessment"), "{}", &prompt[prompt.len().saturating_sub(2000)..]);
    assert!(prompt.contains(&format!("Criterion 1: {RECORD} (requester)")));
    assert!(prompt.contains("full gate passed: 5 stages"));
    assert_eq!(wire["obligationAssessments"][0]["carried"], json!(false), "{wire}");
    let notice = wire["notice"].as_str().unwrap();
    assert!(notice.contains(&format!("{RECORD}: read whole (49 rows) but not carried")), "{notice}");
}
