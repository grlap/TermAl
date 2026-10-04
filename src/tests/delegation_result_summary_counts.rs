// Summary-wording witnesses for the delegation result packet parser.
//
// Owns: tests that a packet whose Findings section is explicitly None never
// gains an actionable finding from its summary wording, whatever numbers or
// finding words the summary contains; that a summary which appears to report
// findings yields a visible, non-actionable inconsistency note instead; and
// that a clean summary yields neither.
// Does not own: the general packet parsing tests, including the recovery of
// structured preamble findings, which stay in delegation_result_parser.rs,
// from which this module is declared.

use super::*;

/// Verbatim full output of a completed acceptance evaluator whose compact
/// packet was given a false "Unspecified" finding (1920 bytes, sha256
/// bb021172d0d0698247ed25d04444577ea0acb5762ac5b91a513165092dd5d267).
const EVALUATOR_OUTPUT_CRITERION_PASS: &str =
    include_str!("fixtures/evaluator-output-findings-none-criterion-pass.txt");

/// A second verbatim evaluator output of the same shape (2307 bytes, sha256
/// 793348b74a00e622a52f4c4ce3e86475eeab13cb83b1a45b1340b191461d4ff7).
const EVALUATOR_OUTPUT_REGRESSION_PASS: &str =
    include_str!("fixtures/evaluator-output-findings-none-regression-pass.txt");

const INCONSISTENCY_NOTE: &str = "Inconsistent result packet";

fn packet_with_summary(summary: &str) -> String {
    format!("## Result\n\nStatus: completed\n\nSummary:\n{summary}\n\nFindings:\n- None")
}

fn inconsistency_notes(parsed: &ParsedDelegationResult) -> Vec<&String> {
    parsed
        .notes
        .iter()
        .filter(|note| note.starts_with(INCONSISTENCY_NOTE))
        .collect()
}

#[test]
fn evaluator_output_with_findings_none_and_a_criterion_count_yields_no_finding() {
    let parsed = parse_delegation_result_packet(EVALUATOR_OUTPUT_CRITERION_PASS)
        .expect("the captured evaluator output should parse");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert!(
        parsed.findings.is_empty(),
        "an explicit Findings: None must not gain an actionable finding: {:?}",
        parsed.findings
    );
}

#[test]
fn evaluator_output_with_findings_none_and_a_regression_criterion_yields_no_finding() {
    let parsed = parse_delegation_result_packet(EVALUATOR_OUTPUT_REGRESSION_PASS)
        .expect("the captured evaluator output should parse");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert!(
        parsed.findings.is_empty(),
        "an explicit Findings: None must not gain an actionable finding: {:?}",
        parsed.findings
    );
}

/// Summaries that report nothing: neither a finding nor an inconsistency note.
#[test]
fn zero_negated_label_and_tally_counts_do_not_contradict_findings_none() {
    for summary in [
        "The gate ran: 3878 passed, 0 failures.",
        "3878 passed; 0 failed; 24 ignored.",
        "No gaps remain.",
        "Criterion 2: pass. The regression test covers the change.",
        "Checked line 308 for the failure path; nothing to report.",
        "Step 3 covers the error path.",
        "3878 passed, 24 ignored, and the failure count is 0.",
        "All 3906 tests passed, 0 failures.",
        "Ran 12 checks, 0 errors.",
        "Checked lines 10-20 for the failure path.",
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a clean packet should parse");
        assert!(
            parsed.findings.is_empty(),
            "summary {summary:?} gained an actionable finding: {:?}",
            parsed.findings
        );
        assert!(
            inconsistency_notes(&parsed).is_empty(),
            "summary {summary:?} gained an inconsistency note: {:?}",
            parsed.notes
        );
    }
}

/// Every counterexample raised by review pair rounds 1 to 3, whichever way
/// the summary heuristic reads it: summary wording alone never yields an
/// actionable finding when the Findings section is explicitly None.
#[test]
fn summary_wording_alone_never_yields_an_actionable_finding() {
    for summary in [
        // Round 1.
        "Found one fail-open issue.",
        "Found two failing regressions.",
        // Round 2.
        "Found one ignored security issue.",
        "Found one failed concurrency regression.",
        "All 3906 tests passed, 0 failures.",
        "Ran 12 checks, 0 errors.",
        "Checked lines 10-20 for the failure path.",
        // Round 3.
        "Issues found: 2.",
        "Found 2: an issue in X and a race in Y.",
        "Found two\nissues in the parser.",
        "Found 2:\n- an issue in X\n- a race in Y",
        "Found 2 (High severity).",
        "Found one issue (High).",
        "Found issues: 1 High, 1 Medium.",
        "Findings (2): both Medium.",
        "Found one issue, High severity.",
        "Version 1.2 regression tests passed.",
        "Validated the 0.5% error budget.",
        "High throughput verified; 0 issues.",
        "Found 12 high-severity test cases, 0 issues.",
        "Section 3.2 describes the failure policy.",
        "Steps 3 and 4 exercise the failure path.",
        "The 2 regression tests pass.",
        "Test 2 covers the failure path.",
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("the packet should parse");
        assert!(
            parsed.findings.is_empty(),
            "summary {summary:?} gained an actionable finding: {:?}",
            parsed.findings
        );
    }
}

/// A summary that appears to report findings against an explicit None yields
/// one visible, non-actionable note that names the contradiction, any declared
/// severity, and the full output.
#[test]
fn a_summary_that_counts_findings_yields_the_inconsistency_note() {
    for (summary, declared) in [
        ("Found one High-severity issue.", Some("High")),
        ("Found one low-severity issue; no high-risk areas remain.", Some("Low")),
        ("Criterion 2 passes, but two issues remain.", None),
        ("3878 passed, but one regression was found.", None),
        ("Found one fail-open issue.", None),
        ("Found two failing regressions.", None),
        ("Found one failed regression.", None),
        ("Found one ignored security issue.", None),
        ("Found one failed concurrency regression.", None),
        ("Two tests failed and one regression remains.", None),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a contradictory packet should parse");
        assert!(
            parsed.findings.is_empty(),
            "summary {summary:?} gained an actionable finding: {:?}",
            parsed.findings
        );
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
        assert!(notes[0].contains("Findings section says None"), "summary {summary:?}");
        assert!(notes[0].contains("Read the full output"), "summary {summary:?}");
        match declared {
            Some(severity) => assert!(
                notes[0].contains(&format!("declared severity: {severity}")),
                "summary {summary:?}: {}",
                notes[0]
            ),
            None => assert!(
                !notes[0].contains("declared severity"),
                "summary {summary:?}: {}",
                notes[0]
            ),
        }
    }
}
