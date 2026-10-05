// Obligation assessments in the tracker's summary view, read into the
// acceptance evaluator brief and the evaluation request result.
//
// Owns: tests that the assessment reader takes both shapes the tracker serves
// for one verification record: the summary view (`counts` and the `must_show`
// rows, paged by `continuation`, with `history` as the full listing) and the
// old shape (`rows`, `earlier`, `shown`), which the history view keeps; all on
// real pages captured from the tracker.
// Does not own: the old-shape reading, fitting and naming rules, whose tests
// stay in acceptance_obligation_assessment.rs; this module is declared from
// there and reuses its fixtures. No live tracker is touched.

use super::*;

// Pages captured from the tracker build that serves the summary view (Engram
// executable sha256 a914ea5c6dcc2469e80e1224acebc8d2233ac986038e068020c240ad703d9dec),
// reading one snapshot of a real store. A first page is the `show --note
// --json` receipt, with the assessment under `note.assessment`; a
// continuation or history page carries it at the top level.

/// One record (133 rows, 9 of them must_show) whose summary is one page
/// (sha256 9968b930fe373f47de9e477a2501064672d71b16fc3471d0bcb59956bc8b8e04).
const ONE_PAGE_SUMMARY: &str =
    include_str!("fixtures/obligation-assessment-summary/299eb83a-summary-1.json");
/// The first page of the same record's history view, read through its
/// `history` command (sha256 48c07f5d5694be184ffc46d48e8910f1e93749b1dd366e03172a1c78075a55ad).
const ONE_PAGE_HISTORY: &str =
    include_str!("fixtures/obligation-assessment-summary/299eb83a-history-1.json");
const ONE_PAGE_RECORD: &str = "299eb83ab63343998fccfd280c75b08c";

/// Another record (77 rows, 37 of them must_show) whose summary takes two
/// pages, the second read by the first's `continuation`, and whose counts hold
/// an entry with no reason (sha256 fefafc83c514908ff1c9fb9eab9630bda30af713f7e46738eb8fbb834051f932
/// and 7da526e50e53ba11c0e9ef6fed499b9761d649415959dc983bf6207c27d1a652).
const TWO_PAGE_FIRST: &str =
    include_str!("fixtures/obligation-assessment-summary/385e6032-summary-1.json");
const TWO_PAGE_SECOND: &str =
    include_str!("fixtures/obligation-assessment-summary/385e6032-summary-2.json");
/// The first page of that record's history view
/// (sha256 0dcf2fb8a9028091644738b02a51c5b5eb8e009528cf124b985f12a8292e6a97).
const TWO_PAGE_HISTORY: &str =
    include_str!("fixtures/obligation-assessment-summary/385e6032-history-1.json");
const TWO_PAGE_RECORD: &str = "385e6032ee5c4ff68a94f3ed60acfc7a";

/// The one-page record's first page from the installed tracker build that
/// serves only the old shape
/// (sha256 adc3089b2d52995402b1af4a69fc8d3e61c1a1bfa5e8f35740c85c17ea7aaba7).
const OLD_SHAPE_FIRST: &str =
    include_str!("fixtures/obligation-assessment-summary/installed-old-shape.json");

/// The assessment a captured receipt carries: under `note` on the first page,
/// at the top level on a continuation or history page.
fn captured_assessment(text: &str) -> Value {
    let page: Value = serde_json::from_str(text).expect("a captured page is JSON");
    page.get("note")
        .and_then(|note| note.get("assessment"))
        .or_else(|| page.get("assessment"))
        .cloned()
        .expect("a captured page carries an assessment")
}

/// The `--after` token of a tracker command.
fn after_token(command: &Value) -> String {
    let command = command.as_str().expect("a continuation command");
    let mut words = command.split_whitespace();
    words.find(|word| *word == "--after").expect("an --after token");
    words.next().expect("a token after --after").to_owned()
}

/// Requests an independent evaluation whose criterion 1 cites `record`. Its
/// exact read carries `first` as the record's assessment; an `--after` read
/// returns the page `pages` maps its token to, and any other token fails as a
/// transport failure.
fn evaluate_record(record: &'static str, first: Value, pages: Vec<(String, Value)>) -> (Value, String) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "obligation-summary-view-runtime",
    );
    let request = serde_json::from_value(json!({"workRef": "w-task", "agent": "Codex",
        "criterionEvidence": [{"criterion": 1, "locators": [record]}]}))
    .unwrap();
    let show = show_receipt(None);
    let mut note = json!({
        "locator": record, "kind": "verification", "family": "notes",
        "by": "peer-holder (agent=claude)", "created_at": "2026-10-05T01:18:16Z",
        "summary": "`node scripts/test-launcher.mjs full` exited 0\nfull gate passed: 5 stages",
        "body_bytes": 617, "non_holder": false,
        "verification": {"check_kind": "test", "producer_outcome": "succeeded",
            "result": "passed", "source_revision": "content-v1:e053bbdf"}
    });
    note["assessment"] = first;
    let response = state
        .request_acceptance_evaluation_with_runner(&parent, request, move |_, args, _| {
            if let Some(at) = args.iter().position(|arg| arg == "--note") {
                assert_eq!(args[at + 1], record, "only the cited record is read");
                return match args.iter().position(|arg| arg == "--after") {
                    None => Ok(json!({"work_ref": "w-task", "note": note.clone()})),
                    Some(after) => pages
                        .iter()
                        .find(|(token, _)| *token == args[after + 1])
                        .map(|(_, page)| {
                            json!({"work_ref": "w-task", "locator": record, "assessment": page})
                        })
                        .ok_or_else(|| EngramTransportError::transport("the tracker process exited")),
                };
            }
            if args.iter().any(|arg| arg == "inspect") {
                Ok(super::super::evidence_selection::canonical_core_receipt())
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
    (wire, prompt)
}

/// The two-page record's second page, keyed by the token of the first page's
/// continuation.
fn two_page_continuation() -> Vec<(String, Value)> {
    let first = captured_assessment(TWO_PAGE_FIRST);
    vec![(after_token(&first["continuation"]), captured_assessment(TWO_PAGE_SECOND))]
}

fn has_group(summary: &Value, status: &str, reason: &str, count: u64) -> bool {
    summary["groups"].as_array().is_some_and(|groups| {
        groups.iter().any(|group| {
            group["status"] == json!(status)
                && group["reason"] == json!(reason)
                && group["count"] == json!(count)
        })
    })
}

/// A summary-view assessment is rendered from its counts and its must_show
/// rows: every must_show row in full, every other row only counted, and the
/// tracker's `history` command as the full history at the cut.
#[test]
fn a_summary_view_assessment_is_rendered_from_its_counts_and_must_show_rows() {
    let first = captured_assessment(ONE_PAGE_SUMMARY);

    let (wire, prompt) = evaluate_record(ONE_PAGE_RECORD, first.clone(), Vec::new());

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["locator"], json!(ONE_PAGE_RECORD), "{wire}");
    assert_eq!(summary["complete"], json!(true), "{summary}");
    assert!(summary.get("incompleteReason").is_none(), "{summary}");
    assert_eq!(summary["total"], json!(133), "{summary}");
    assert_eq!(summary["recordPosition"], json!(85), "{summary}");
    assert_eq!(summary["cutPosition"], json!(86), "{summary}");
    assert_eq!(summary["mustShowTotal"], json!(9), "{summary}");
    assert_eq!(summary["rowsRead"], json!(9), "{summary}");
    let rows = summary["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 9, "{summary}");
    assert_eq!(
        rows.iter().filter(|row| row["status"] == json!("mismatch")).count(),
        6,
        "the six mismatches: {summary}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["status"] == json!("left_out") && row["recorded"] == json!("open"))
            .count(),
        3,
        "the three left-out rows still open: {summary}"
    );
    assert!(has_group(summary, "left_out", "already_closed", 2), "{summary}");
    assert!(has_group(summary, "left_out", "not_yet_defined", 125), "{summary}");
    assert!(has_group(summary, "mismatch", "stale_source_revision", 6), "{summary}");
    assert_eq!(summary["historyContinuation"], first["history"], "{summary}");
    assert_eq!(summary["carried"], json!(true), "{summary}");

    assert!(
        prompt.contains(&format!(
            "Obligation assessment of {ONE_PAGE_RECORD} (record position 85, cut position 86): 133 rows, read whole: the 9 must_show rows are listed below, the rest only counted."
        )),
        "{prompt}"
    );
    assert!(prompt.contains("125 left_out (not_yet_defined)"), "{prompt}");
    assert!(
        prompt.contains("for a record in the tracker's summary view, its must_show rows are listed in full"),
        "{prompt}"
    );
    assert_eq!(rendered_rows(&prompt).len(), 9, "{prompt}");
    assert!(
        prompt.contains(&format!(
            "Full history at this cut, oldest first: `{}`",
            first["history"].as_str().unwrap()
        )),
        "{prompt}"
    );
}

/// A summary that takes two pages is read through `continuation` to the page
/// that names none; a counts entry for a status that has no reason carries
/// none.
#[test]
fn a_summary_view_assessment_read_through_its_continuation_is_whole() {
    let first = captured_assessment(TWO_PAGE_FIRST);

    let (wire, prompt) = evaluate_record(TWO_PAGE_RECORD, first.clone(), two_page_continuation());

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["complete"], json!(true), "{summary}");
    assert!(summary.get("resume").is_none(), "{summary}");
    assert_eq!(summary["total"], json!(77), "{summary}");
    assert_eq!(summary["mustShowTotal"], json!(37), "{summary}");
    assert_eq!(summary["rowsRead"], json!(37), "{summary}");
    assert_eq!(summary["rows"].as_array().map(Vec::len), Some(37), "{summary}");
    assert!(
        summary["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| group == &json!({"status": "matches", "count": 37})),
        "{summary}"
    );
    assert!(has_group(summary, "left_out", "not_yet_defined", 40), "{summary}");
    assert_eq!(summary["historyContinuation"], first["history"], "{summary}");
    assert!(prompt.contains("37 matches, 40 left_out (not_yet_defined)"), "{prompt}");
    assert_eq!(rendered_rows(&prompt).len(), 37, "{prompt}");
}

/// An incomplete summary-view read says how many must_show rows were read of
/// the must_show total and of all rows, why it stopped, where to resume at the
/// same cut, and that the tracker's `history` command is the full history.
#[test]
fn an_incomplete_summary_view_read_names_its_must_show_rows_read_and_its_history_command() {
    let first = captured_assessment(TWO_PAGE_FIRST);

    let (wire, prompt) = evaluate_record(TWO_PAGE_RECORD, first.clone(), Vec::new());

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["incompleteReason"], json!("transport_failure"), "{summary}");
    assert_eq!(summary["rowsRead"], json!(35), "{summary}");
    assert_eq!(summary["resume"], first["continuation"], "{summary}");
    let resume = first["continuation"].as_str().unwrap();
    let history = first["history"].as_str().unwrap();
    let notice = wire["notice"].as_str().unwrap();
    assert!(
        notice.contains(&format!(
            "{TWO_PAGE_RECORD}: incomplete, 35 of 37 must_show rows read, of 77 rows (transport_failure)"
        )),
        "{notice}"
    );
    assert!(notice.contains(&format!("resume at the same cut with `{resume}`")), "{notice}");
    assert!(notice.contains(&format!("is `{history}` and then each page's continuation")), "{notice}");
    assert_eq!(summary["carried"], json!(true), "{summary}");
    assert!(
        prompt.contains("INCOMPLETE: 35 of 37 must_show rows read, of 77 rows (transport_failure). The counts cover every row; an unread must_show row may be open or a mismatch."),
        "{prompt}"
    );
    assert!(prompt.contains(&format!("Resume at the same cut: `{resume}`")), "{prompt}");
    assert!(
        prompt.contains(&format!("Full history at this cut, oldest first: `{history}`")),
        "{prompt}"
    );
    assert_eq!(rendered_rows(&prompt).len(), 35, "every must_show row read is listed whole");
}

/// A summary page the reader cannot trust whole stops the read as
/// malformed_page, with nothing from it counted or listed.
#[test]
fn a_malformed_summary_page_stops_the_read() {
    let cases: [(&str, fn(&mut Value)); 11] = [
        ("an unknown view", |page| page["view"] = json!("digest")),
        ("a missing must_show total", |page| {
            page.as_object_mut().unwrap().remove("must_show_total");
        }),
        ("a negative must_show offset", |page| page["must_show_earlier"] = json!(-1)),
        ("a total that is not a number", |page| page["total"] = json!("133")),
        ("counts that are not an array", |page| page["counts"] = json!({})),
        ("must_show that is not an array", |page| page["must_show"] = json!({})),
        ("a count entry whose reason is not a string", |page| page["counts"][0]["reason"] = json!(7)),
        ("counts that do not sum to the total", |page| page["counts"][0]["count"] = json!(3)),
        ("more must_show rows than the must_show total", |page| page["must_show_total"] = json!(5)),
        ("a must_show total above the total", |page| {
            page["must_show_total"] = json!(134);
            page["must_show_remaining"] = json!(125);
        }),
        ("two count entries for one status and reason", |page| {
            let duplicate = page["counts"][0].clone();
            let counts = page["counts"].as_array_mut().unwrap();
            counts.push(json!({"status": duplicate["status"], "reason": duplicate["reason"], "count": 0}));
        }),
    ];
    for (case, break_page) in cases {
        let mut first = captured_assessment(ONE_PAGE_SUMMARY);
        break_page(&mut first);

        let (wire, _) = evaluate_record(ONE_PAGE_RECORD, first, Vec::new());

        let summary = &wire["obligationAssessments"][0];
        assert_eq!(summary["incompleteReason"], json!("malformed_page"), "{case}: {summary}");
        assert_eq!(summary["rowsRead"], json!(0), "{case}: {summary}");
        assert_eq!(summary["rows"], json!([]), "{case}: {summary}");
        assert_eq!(summary["groups"], json!([]), "{case}: {summary}");
    }
}

/// A first summary page rejected for a reason found after its counts and
/// totals are read (a must_show offset that does not start the read, or a
/// must_show row that cannot be shown whole) leaves nothing of itself in the
/// summary: no counts, no totals, no rows.
#[test]
fn a_rejected_first_summary_page_leaves_nothing_counted() {
    let cases: [(&str, &str, fn(&mut Value)); 2] = [
        ("a must_show offset that does not start the read", "assessment_changed", |page| {
            page["must_show_earlier"] = json!(1);
            page["must_show_remaining"] = json!(1);
        }),
        ("a must_show row that cannot be shown whole", "malformed_row", |page| {
            page["must_show"][0]["rule"] = json!("r".repeat(300));
        }),
    ];
    for (case, reason, break_page) in cases {
        let mut first = captured_assessment(TWO_PAGE_FIRST);
        break_page(&mut first);

        let (wire, _) = evaluate_record(TWO_PAGE_RECORD, first, Vec::new());

        let summary = &wire["obligationAssessments"][0];
        assert_eq!(summary["incompleteReason"], json!(reason), "{case}: {summary}");
        assert_eq!(summary["rowsRead"], json!(0), "{case}: {summary}");
        assert_eq!(summary["rows"], json!([]), "{case}: {summary}");
        assert_eq!(summary["groups"], json!([]), "{case}: {summary}");
        assert_eq!(summary["total"], Value::Null, "{case}: {summary}");
        assert!(summary.get("mustShowTotal").is_none(), "{case}: {summary}");
    }
}

/// A later summary page that is not from the first page's cut (other counts,
/// another must_show total, other positions or total, or an offset that
/// repeats rows already read), or that carries a row it cannot show whole,
/// stops the read at the cursor that fetched it. The first page's counts and
/// rows stay exactly as read, and nothing of the rejected page is added.
#[test]
fn a_later_summary_page_from_another_cut_or_with_a_bad_row_stops_the_read() {
    let cases: [(&str, &str, fn(&mut Value)); 6] = [
        ("other counts", "assessment_changed", |page| {
            page["counts"][0]["count"] = json!(41);
            page["counts"][1]["count"] = json!(36);
        }),
        ("another must_show total", "assessment_changed", |page| {
            page["must_show_total"] = json!(38);
            page["must_show_remaining"] = json!(1);
        }),
        ("another cut position", "assessment_changed", |page| page["cut_position"] = json!(508)),
        ("another total", "assessment_changed", |page| {
            page["total"] = json!(78);
            page["counts"][0]["count"] = json!(41);
        }),
        ("an offset that repeats rows already read", "assessment_changed", |page| {
            page["must_show_earlier"] = json!(34);
            page["must_show_remaining"] = json!(1);
        }),
        ("a row it cannot show whole", "malformed_row", |page| {
            page["must_show"][0]["rule"] = json!("r".repeat(300));
        }),
    ];
    let first = captured_assessment(TWO_PAGE_FIRST);
    let token = after_token(&first["continuation"]);
    for (case, reason, break_page) in cases {
        let mut second = captured_assessment(TWO_PAGE_SECOND);
        break_page(&mut second);

        let (wire, _) = evaluate_record(TWO_PAGE_RECORD, first.clone(), vec![(token.clone(), second)]);

        let summary = &wire["obligationAssessments"][0];
        assert_eq!(summary["incompleteReason"], json!(reason), "{case}: {summary}");
        assert_eq!(summary["rowsRead"], json!(35), "{case}: {summary}");
        assert_eq!(summary["rows"].as_array().map(Vec::len), Some(35), "{case}: {summary}");
        assert_eq!(summary["total"], json!(77), "{case}: {summary}");
        assert_eq!(summary["mustShowTotal"], json!(37), "{case}: {summary}");
        assert_eq!(summary["groups"].as_array().map(Vec::len), Some(2), "{case}: {summary}");
        assert!(has_group(summary, "left_out", "not_yet_defined", 40), "{case}: {summary}");
        assert_eq!(summary["resume"], first["continuation"], "{case}: {summary}");
    }
}

/// The same counts in another order on a later page are the same counts.
#[test]
fn a_later_summary_page_may_list_its_counts_in_another_order() {
    let first = captured_assessment(TWO_PAGE_FIRST);
    let mut second = captured_assessment(TWO_PAGE_SECOND);
    second["counts"].as_array_mut().unwrap().reverse();
    let pages = vec![(after_token(&first["continuation"]), second)];

    let (wire, _) = evaluate_record(TWO_PAGE_RECORD, first, pages);

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["complete"], json!(true), "{summary}");
    assert_eq!(summary["rowsRead"], json!(37), "{summary}");
}

/// One record's read never mixes shapes: the history view's page where the
/// summary view's next page should be stops the read as malformed_page, at the
/// cursor that fetched it.
#[test]
fn a_change_of_shape_mid_read_stops_it_as_malformed() {
    let first = captured_assessment(TWO_PAGE_FIRST);
    let history = captured_assessment(TWO_PAGE_HISTORY);
    let pages = vec![(after_token(&first["continuation"]), history)];

    let (wire, _) = evaluate_record(TWO_PAGE_RECORD, first.clone(), pages);

    let summary = &wire["obligationAssessments"][0];
    assert_eq!(summary["incompleteReason"], json!("malformed_page"), "{summary}");
    assert_eq!(summary["rowsRead"], json!(35), "{summary}");
    assert_eq!(summary["resume"], first["continuation"], "{summary}");
}

/// A history-view page keeps the old shape and is read as it, rows and all.
#[test]
fn a_history_view_page_is_read_as_the_old_shape() {
    let first = captured_assessment(ONE_PAGE_HISTORY);
    assert_eq!(first["view"], json!("history"));

    let (wire, _) = evaluate_record(ONE_PAGE_RECORD, first, Vec::new());

    let summary = &wire["obligationAssessments"][0];
    assert!(summary.get("mustShowTotal").is_none(), "{summary}");
    assert_eq!(summary["total"], json!(133), "{summary}");
    assert_eq!(summary["rowsRead"], json!(8), "{summary}");
    assert_eq!(summary["incompleteReason"], json!("transport_failure"), "{summary}");
}

/// The tracker that serves only the old shape is read as before.
#[test]
fn an_old_shape_page_from_the_installed_tracker_is_read_as_before() {
    let first = captured_assessment(OLD_SHAPE_FIRST);
    assert!(first.get("view").is_none());

    let (wire, _) = evaluate_record(ONE_PAGE_RECORD, first, Vec::new());

    let summary = &wire["obligationAssessments"][0];
    assert!(summary.get("mustShowTotal").is_none(), "{summary}");
    assert_eq!(summary["total"], json!(133), "{summary}");
    assert_eq!(summary["rowsRead"], json!(8), "{summary}");
    assert_eq!(summary["incompleteReason"], json!("transport_failure"), "{summary}");
}
