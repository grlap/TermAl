// Owns how both evaluator briefs word evidence they could not read: the rule
// that names unread evidence by its read cut and paging reason rather than by a
// continuation that can outgrow the rationale limit, the same-session notice
// for unread records with nothing read left out, and the Compact-to-Minimal
// floor of the independent brief. Does not own the omission inventory or its
// paging (acceptance_evaluation_omissions.rs) or criterion evidence selection
// (acceptance_evaluation_evidence.rs). A new module declared from
// acceptance_evaluation.rs; it reuses that module's fixtures and runs no
// tracker process.
use super::*;

const UNREAD_RULE: &str = "name unread evidence by its read cut and paging reason where the brief \
     shows them, never by copying its continuation";

/// The fixture task with `unread` entries the host never read, a paging stop
/// and a read cut, and a captured continuation of `token_chars` characters.
fn task_with_unread(unread: usize, token_chars: usize) -> AcceptanceEvaluationTask {
    let mut task = parse_acceptance_evaluation_task(show_receipt(None), full_receipt()).unwrap();
    task.evidence_omitted = unread;
    task.evidence_paging = AcceptanceEvidencePaging {
        continuation: Some("c".repeat(token_chars)),
        continuation_bytes: token_chars,
        continuation_omitted: false,
        read_cut: Some(json!({"project_position": 42})),
        reason: Some("entry_limit".to_owned()),
    };
    task
}

/// The brief's text with every run of whitespace made one space, so that a
/// rule wrapped across lines reads as the sentence it is.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn both_briefs_name_unread_evidence_by_its_read_cut_not_its_continuation() {
    // A continuation longer than a rationale may be: copying it cannot fit.
    let task = task_with_unread(3, MAX_ACCEPTANCE_EVALUATION_RATIONALE_CHARS + 1_000);
    let independent = one_line(
        &build_acceptance_evaluator_brief(&task, ".", MAX_ACCEPTANCE_BRIEF_BYTES)
            .unwrap()
            .prompt,
    );
    let same_session = one_line(&render_same_session_acceptance_brief_with_detail(
        &task,
        None,
        AcceptanceOmissionDetail::Full,
    ));
    for (brief, text) in [("independent", &independent), ("same-session", &same_session)] {
        assert!(text.contains("paging stopped: entry_limit"), "{brief}: {text}");
        assert!(text.contains(UNREAD_RULE), "{brief} brief rule: {text}");
        assert!(
            !text.contains("locator or captured continuation")
                && !text.contains("locator or continuation"),
            "{brief}: no rule may ask for the continuation in a rationale: {text}"
        );
    }
}

#[test]
fn a_same_session_notice_with_only_unread_records_reports_just_the_boundary() {
    let task = task_with_unread(3, 16);
    let cuts = AcceptanceBriefCuts {
        unread: task.evidence_omitted,
        paging: task.evidence_paging.clone(),
        ..AcceptanceBriefCuts::default()
    };
    for detail in [AcceptanceOmissionDetail::Full, AcceptanceOmissionDetail::Compact] {
        let notice = acceptance_same_session_cut_notice_with_detail("w-task", &cuts, detail)
            .expect("unread records are an omission");
        assert!(
            notice.contains("Unread evidence: 3 entries")
                && notice.contains("paging stopped: entry_limit"),
            "{detail:?}: {notice}"
        );
        assert!(
            !notice.contains("The host read 0 entries") && !notice.contains("()"),
            "{detail:?}: nothing read was left out: {notice}"
        );
    }
    // The same brief built from a task that read nothing says the same.
    let mut empty = task;
    empty.evidence.clear();
    empty.indexed_evidence.clear();
    let brief = render_same_session_acceptance_brief_with_detail(
        &empty,
        None,
        AcceptanceOmissionDetail::Full,
    );
    assert!(brief.contains("Unread evidence: 3 entries"), "{brief}");
    assert!(!brief.contains("The host read 0 entries"), "{brief}");
    // Records read and left out are still named.
    let left_out = AcceptanceBriefCuts {
        left_out: vec!["aaaaaaaa1111".to_owned()],
        ..cuts
    };
    let notice = acceptance_same_session_cut_notice("w-task", &left_out).unwrap();
    assert!(
        notice.contains("The host read 1 entry not carried in this brief (aaaaaaaa1111)."),
        "{notice}"
    );
}

#[test]
fn the_independent_brief_crosses_the_compact_floor_into_minimal_and_refuses_below_it() {
    // A long continuation keeps every Full-detail render above the Compact
    // floor, so the step this test measures is Compact to Minimal.
    let task = task_with_unread(3, 600);
    let floor_at = |detail| {
        render_acceptance_evaluator_brief_with_index_detail(
            &task,
            ".",
            "",
            0,
            0,
            0,
            detail,
            AcceptanceOmissionDetail::Minimal,
            AcceptanceOmissionDetail::Minimal,
        )
        .prompt
        .len()
    };
    let compact = floor_at(AcceptanceOmissionDetail::Compact);
    let minimal = floor_at(AcceptanceOmissionDetail::Minimal);
    assert!(
        minimal < compact,
        "Minimal drops the unread boundary Compact keeps: {minimal} vs {compact}"
    );
    // Below the Compact floor the brief gives way to Minimal, criteria whole.
    for max_bytes in [compact - 1, minimal] {
        let brief = build_acceptance_evaluator_brief(&task, ".", max_bytes)
            .unwrap_or_else(|error| panic!("{max_bytes} bytes fit Minimal: {}", error.message));
        assert_eq!(brief.plan.detail, AcceptanceOmissionDetail::Minimal, "{max_bytes}");
        assert!(brief.prompt.len() <= max_bytes);
        assert!(
            task.criteria
                .iter()
                .all(|criterion| brief.prompt.contains(criterion)),
            "{max_bytes}: {}",
            brief.prompt
        );
    }
    // One byte below the Minimal floor refuses and reports that exact floor.
    let error = build_acceptance_evaluator_brief(&task, ".", minimal - 1)
        .expect_err("below the Minimal floor nothing fits");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains(&format!(
            "the brief is {minimal} bytes against a limit of {}",
            minimal - 1
        )),
        "{}",
        error.message
    );
}
