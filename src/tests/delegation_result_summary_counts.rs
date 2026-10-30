// Summary-wording witnesses for the delegation result packet parser.
//
// Owns: tests that a packet whose Findings section is explicitly None never
// gains an actionable finding from its summary wording, whatever numbers or
// finding words the summary contains; that a summary which appears to report
// findings yields a visible, non-actionable inconsistency note instead; and
// that a clean summary yields neither; that a count the integration review
// set beside a colon, a parenthesis or a wrapped line still recovers the
// structured preamble findings; and that the note caps the summary it repeats.
// Does not own: the general packet parsing tests, including the other
// preamble-recovery tests, which stay in delegation_result_parser.rs, from
// which this module is declared.

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
        ("Found one low-severity issue; no high-risk areas remain.", Some("High")),
        ("Fixed one High-severity issue found earlier, but it remains open.", Some("High")),
        ("Review found, after inspection, one High-severity issue.", Some("High")),
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

#[test]
fn contradiction_note_names_the_highest_severity_in_either_order() {
    for (lower, higher) in [
        ("Low", "Medium"),
        ("Low", "High"),
        ("Low", "Critical"),
        ("Medium", "High"),
        ("Medium", "Critical"),
        ("High", "Critical"),
    ] {
        for (first, second) in [(lower, higher), (higher, lower)] {
            for summary in [
                format!("Found one {first}-severity issue. Found one {second}-severity issue."),
                format!("Found one {first}-severity issue and one {second}-severity issue."),
                format!("Found two issues: one {first}-severity issue, one {second}-severity issue."),
                format!("Found one {first} issue, and one {second} issue."),
                format!("Found one {first}-severity issue, one {second}-severity issue."),
                format!("Found one {first}-severity issue; one {second}-severity issue."),
            ] {
                let parsed = parse_delegation_result_packet(&packet_with_summary(&summary))
                    .expect("a contradictory packet should parse");
                assert!(parsed.findings.is_empty(), "summary {summary:?}");
                let notes = inconsistency_notes(&parsed);
                assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
                assert!(
                    notes[0].contains(&format!("declared severity: {higher}")),
                    "summary {summary:?}: {}",
                    notes[0]
                );
            }
        }
    }
}

#[test]
fn whole_sentence_diagnostics_keep_the_highest_attached_word_in_mixed_prose() {
    for (other, attached) in [
        ("Found no Critical issues.", true),
        ("Did not find one Critical issue.", true),
        ("Did not find even one Critical issue.", true),
        ("Found and resolved one Critical issue.", true),
        ("Fixed one Critical-severity condition found earlier.", true),
        ("Found one module with Critical complexity.", false),
    ] {
        // Whole CLEAN sentences contribute nothing. A comma-joined non-clean
        // sentence contributes its highest attached word, even in a negative
        // or resolved clause. Incidental complexity is still not attached.
        for (summary, expected) in [
            (format!("Found one Low-severity issue. {other}"), "Low"),
            (format!("{other} Found one Low-severity issue."), "Low"),
            (format!("Found one Low-severity issue, {}.", other.trim_end_matches('.')),
                if attached { "Critical" } else { "Low" }),
            (format!("{}, found one Low-severity issue.", other.trim_end_matches('.')),
                if attached { "Critical" } else { "Low" }),
        ] {
            let parsed = parse_delegation_result_packet(&packet_with_summary(&summary))
                .expect("a contradictory packet should parse");
            assert!(parsed.findings.is_empty(), "summary {summary:?}");
            let notes = inconsistency_notes(&parsed);
            assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
            assert!(
                notes[0].contains(&format!("declared severity: {expected}")),
                "summary {summary:?}: {}",
                notes[0]
            );
        }
    }
}

#[test]
fn clean_or_resolved_mixed_severities_do_not_gain_a_contradiction_note() {
    for summary in [
        "No Low issues. No Critical issues.",
        "Fixed one Low-severity issue found earlier. Fixed one Critical-severity issue found earlier.",
        "Found and resolved one Critical issue. Found and resolved one Low issue.",
        "Did not find one Critical issue. Did not find one Low issue.",
        "Did not find even one Critical issue.",
        "Did not find one Low issue and one Critical issue.",
        "Found one High-severity issue, now fixed.",
        "Found one High-severity race, fixed it.",
        "Found and resolved one High-severity issue and one Low-severity issue.",
        "Found one module with Low complexity. Found one module with Critical complexity.",
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a clean packet should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        assert!(
            inconsistency_notes(&parsed).is_empty(),
            "summary {summary:?}: {:?}",
            parsed.notes
        );
    }
}

#[test]
fn ambiguous_prose_keeps_a_visible_diagnostic_without_inventing_a_finding() {
    for (summary, severity) in [
        // A possibility of resolution is not a definite resolution.
        ("Found one High-severity issue, possibly fixed.", "High"),
        ("Found one High-severity issue and one Low-severity issue, now fixed.", "High"),
        ("Fixed one Low-severity issue, found one Critical-severity issue.", "Critical"),
        ("Fixed one Critical-severity issue, found one Low-severity issue.", "Critical"),
        // A non-clean joined sentence keeps its highest attached severity;
        // a negative comma clause cannot silence the entire sentence.
        ("Did not find one Critical issue, one Low issue remains.", "Critical"),
        ("Found not only one Low-severity issue but also one Critical-severity issue.", "Critical"),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("an ambiguous packet should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
        assert!(
            notes[0].contains(&format!("declared severity: {severity}")),
            "summary {summary:?}: {}",
            notes[0]
        );
    }
}

#[test]
fn separate_discoveries_and_unrelated_negation_keep_the_diagnostic_visible() {
    for (summary, severity) in [
        ("Fixed one issue, found one High-severity issue.", Some("High")),
        ("Fixed one Low-severity issue, found one new issue.", Some("Low")),
        ("Found one High-severity issue that the patch has not fixed.", Some("High")),
        ("Found one High-severity issue in the dispatcher; the earlier parser race was fixed.", Some("High")),
        ("Not ready to land: found one High-severity issue.", Some("High")),
        ("Found without doubt one High-severity issue.", Some("High")),
        ("Found in parser.rs one High-severity issue.", Some("High")),
        ("Found in version 1.2 one High-severity issue.", Some("High")),
        ("Found one High-severity issue that was never resolved.", Some("High")),
        ("Found one High-severity issue that has not been addressed.", Some("High")),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a contradictory packet should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
        if let Some(severity) = severity {
            assert!(
                notes[0].contains(&format!("declared severity: {severity}")),
                "summary {summary:?}: {}",
                notes[0]
            );
        } else {
            assert!(!notes[0].contains("declared severity"), "{}", notes[0]);
        }
    }
}

#[test]
fn explicit_suppress_list_keeps_clear_negated_resolved_and_clean_summaries_empty() {
    for summary in [
        "No High issues.",
        "None of the High issues were found.",
        "Zero High issues.",
        "0 High issues.",
        "Did not find even one High-severity issue.",
        "High-severity issue not found.",
        "Fixed one High-severity issue.",
        "Resolved one High-severity issue.",
        "Addressed one High-severity issue.",
        "One High-severity issue has been fixed.",
        "One High-severity issue, now resolved.",
        "One High-severity issue, addressed it.",
        "The High-severity review is clean.",
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a suppressed packet should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        assert!(
            inconsistency_notes(&parsed).is_empty(),
            "summary {summary:?}: {:?}",
            parsed.notes
        );
    }
}

#[test]
fn novel_phrasing_defaults_to_a_visible_note_without_a_count_or_discovery_verb() {
    for (summary, severity) in [
        ("A High issue awaits triage.", "High"),
        ("Dispatcher manifests a Critical-severity fault.", "Critical"),
        ("Low issue catalogued; a Critical-severity condition materializes.", "Critical"),
        // A modal may describe impact rather than resolution. An extra note is
        // the accepted error direction; it never invents a structured finding.
        ("Fixed one High-severity issue that may have caused crashes.", "High"),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("an unfamiliar summary should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
        assert!(
            notes[0].contains(&format!("declared severity: {severity}")),
            "summary {summary:?}: {}",
            notes[0]
        );
    }
}

#[test]
fn negated_or_unrelated_resolution_and_partial_clean_clauses_remain_visible() {
    for (summary, severity) in [
        ("Have not fixed the High-severity issue.", "High"),
        ("Previously fixed code contains a High-severity issue.", "High"),
        ("Fixed the tests but introduced one High-severity regression.", "High"),
        ("Addressed the earlier comments and one Critical race now blocks landing.", "Critical"),
        ("Fixed X but there is one High-severity issue.", "High"),
        ("One High-severity issue remains but everything else is clean.", "High"),
        ("One High-severity issue remains but the rest is clean.", "High"),
        ("One High-severity issue but otherwise the diff is clean.", "High"),
        ("Fixed one Low issue and one Critical race now blocks landing.", "Critical"),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a contradictory packet should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
        assert!(
            notes[0].contains(&format!("declared severity: {severity}")),
            "summary {summary:?}: {}",
            notes[0]
        );
    }
}

#[test]
fn discovery_negation_distinguishes_the_review_from_a_third_party_that_missed_a_finding() {
    for (summary, severity) in [
        ("Did not detect one High-severity race.", None),
        ("I did not detect one High-severity race.", None),
        ("We did not flag one Critical regression.", None),
        ("The review did not find one High-severity issue.", None),
        ("The existing tests did not detect one High-severity race in the dispatcher.", Some("High")),
        ("CI did not flag one Critical regression.", Some("Critical")),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a packet with negated discovery should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        let notes = inconsistency_notes(&parsed);
        if let Some(severity) = severity {
            assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
            assert!(
                notes[0].contains(&format!("declared severity: {severity}")),
                "summary {summary:?}: {}",
                notes[0]
            );
        } else {
            assert!(notes.is_empty(), "summary {summary:?}: {:?}", parsed.notes);
        }
    }
}

#[test]
fn direct_resolution_synonyms_and_without_errors_keep_the_clean_controls_empty() {
    let mut summaries = vec![
        "All 3906 tests passed without failures.".to_owned(),
        "Ran 3 suites without errors.".to_owned(),
    ];
    for resolution in [
        "addressed", "closed", "corrected", "eliminated",
        "fixed", "removed", "repaired", "resolved",
    ] {
        summaries.extend([
            format!("{resolution} one High-severity issue."),
            format!("Found and {resolution} one High-severity issue."),
            format!("One High-severity issue was {resolution}."),
            format!("Found one High-severity issue was {resolution}."),
        ]);
    }
    for summary in summaries {
        let parsed = parse_delegation_result_packet(&packet_with_summary(&summary))
            .expect("a clean or directly resolved packet should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        assert!(
            inconsistency_notes(&parsed).is_empty(),
            "summary {summary:?}: {:?}",
            parsed.notes
        );
    }
}

#[test]
fn mixed_zero_and_positive_counts_remain_visible_under_the_sentence_contract() {
    for (summary, severity) in [
        ("No Low issues — one Critical-severity race remains.", "Critical"),
        ("No Low issues: one Critical-severity race remains.", "Critical"),
        // A non-clean joined table, like comma-joined prose, retains its
        // highest attached word. Only an entirely zero table is CLEAN.
        ("Found 0 Critical / 2 High issues.", "Critical"),
        ("Found 0 Critical / 0 High / 2 Medium / 1 Low findings.", "Critical"),
        ("Critical 0 | High 1 | Medium 0 issues", "Critical"),
        ("None of the High-severity issues were addressed.", "High"),
        ("None of the three High issues has been fixed.", "High"),
        ("No Low issues and one Critical-severity issue remains.", "Critical"),
        ("No Low issues and a Critical-severity race remains.", "Critical"),
        ("No Low issues and Critical-severity race remains.", "Critical"),
        ("Found zero Low issues but two High-severity races remain.", "High"),
        ("No Low issues – one Critical-severity race remains.", "Critical"),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a packet with separate severity mentions should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
        assert!(
            notes[0].contains(&format!("declared severity: {severity}")),
            "summary {summary:?}: {}",
            notes[0]
        );
    }
}

#[test]
fn negated_discovery_does_not_hide_a_different_object_or_unresolved_mention() {
    for (summary, severity) in [
        ("Did not find one Low issue although one Critical-severity race remains.", "Critical"),
        ("Did not find new issues and the earlier High-severity race remains open.", "High"),
        ("Did not find a fix for one Critical regression.", "Critical"),
        ("Could not find the root cause of one High-severity race.", "High"),
        ("Did not find one Low issue: one Critical-severity race remains.", "Critical"),
        ("Did not find one Low issue — one Critical-severity race remains.", "Critical"),
        ("Did not find one Low issue / one Critical-severity race remains.", "Critical"),
        ("Did not find one Low issue, but one Critical-severity race still blocks landing.", "Critical"),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a packet with an unresolved mention should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "summary {summary:?}: {:?}", parsed.notes);
        assert!(
            notes[0].contains(&format!("declared severity: {severity}")),
            "summary {summary:?}: {}",
            notes[0]
        );
    }
}

#[test]
fn negative_mentions_keep_their_own_count_and_direct_object_controls_clean() {
    for summary in [
        "No Low issues.",
        "No Critical-severity races remain.",
        "Found 0 Critical / 0 High issues.",
        "Found 0 Critical / 0 High / 0 Medium / 0 Low findings.",
        "Critical 0 | High 0 | Medium 0 issues",
        "None of the High-severity issues were found.",
        "None of the three High issues were found.",
        "No Low issues and no Critical-severity issues.",
        "Did not find one Low issue.",
        "Did not find even one Critical-severity race.",
        "Did not find new issues and one High-severity race.",
        "Could not find one High-severity race.",
        "Did not find one Low issue and one Critical-severity race.",
        "High-severity race was not found.",
        "No Low issues: no Critical-severity races.",
        "No Low issues — no Critical-severity races.",
        "No Low issues – no Critical-severity races.",
        "No Low issues / no Critical-severity races.",
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a clearly negative packet should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        assert!(
            inconsistency_notes(&parsed).is_empty(),
            "summary {summary:?}: {:?}",
            parsed.notes
        );
    }
}

#[test]
fn closed_sentence_classes_cover_every_negative_scope_review_witness() {
    use DelegationSummarySentenceClass::{Clean, Positive, Unresolved};
    for (summary, class, severity) in [
        ("Found 0 Low issues and a Critical-severity race.", Unresolved, Some("Critical")),
        ("Found 0 Critical / 2 High issues.", Unresolved, Some("Critical")),
        ("Found 0 Critical / 0 High / 2 Medium / 1 Low findings.", Unresolved, Some("Critical")),
        ("Critical 0 | High 1 | Medium 0 issues", Unresolved, Some("Critical")),
        ("No Low issues and a High-severity race.", Unresolved, Some("High")),
        ("No Low issues and the High-severity race found.", Unresolved, Some("High")),
        ("The patch is not without a High-severity issue.", Unresolved, Some("High")),
        ("Addressed none of the High-severity issues.", Unresolved, Some("High")),
        ("Addressed none of the High issues.", Unresolved, Some("High")),
        ("Fixed none of the three High issues.", Unresolved, Some("High")),
        ("The patch fixes none of the Critical races.", Unresolved, Some("Critical")),
        ("CI caught none of the Critical regressions.", Unresolved, Some("Critical")),
        ("Tests cover none of the High-severity races.", Unresolved, Some("High")),
        ("Fixed no High-severity issues.", Unresolved, Some("High")),
        ("After upgrading to 2.0 High-severity regressions remain.", Unresolved, Some("High")),
        ("Found in version 2.0 a High-severity issue.", Unresolved, Some("High")),
        ("Found none of the High-severity issues.", Clean, None),
        ("Found none of the High issues.", Clean, None),
        ("Found none of the three High issues.", Clean, None),
        ("Without a High-severity issue.", Clean, None),
        ("Without High-severity issues.", Clean, None),
        ("Found 0 Low issues and no Critical-severity race.", Clean, None),
        // An unfamiliar version-prefixed negative is deliberately UNRESOLVED,
        // rather than a new prefix/polarity exception to the closed templates.
        ("Version 2.0 has no High issues.", Unresolved, Some("High")),
        ("No High-severity issues found in the parser.", Unresolved, Some("High")),
        ("No issues found in 3 files.", Unresolved, None),
        ("Found no issues in 2 files.", Unresolved, None),
        ("CI is green; did not flag one Critical regression.", Unresolved, Some("Critical")),
        ("CI is green, did not flag one Critical regression.", Unresolved, Some("Critical")),
        ("CI is green: did not flag one Critical regression.", Unresolved, Some("Critical")),
        ("Found one High-severity fail-open condition.", Positive, Some("High")),
        ("Identified two Medium-severity issues.", Positive, Some("Medium")),
        ("Detected one Low-severity race.", Positive, Some("Low")),
        ("FOUND ONE HIGH–SEVERITY ISSUE.", Positive, Some("High")),
        ("A High—severity issue awaits triage.", Unresolved, Some("High")),
        ("No High–severity issues.", Clean, None),
        ("Found one High.", Positive, Some("High")),
        ("One High.", Unresolved, Some("High")),
        ("Found two High conditions.", Positive, Some("High")),
        ("A High-severity item awaits triage.", Unresolved, Some("High")),
    ] {
        let sentences = delegation_result_note_sentences(summary);
        assert_eq!(sentences.len(), 1, "summary {summary:?}");
        let owned = delegation_result_note_words(sentences[0]);
        let words = owned.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(delegation_result_classify_summary_sentence(&words), class, "{summary:?}");
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary))
            .expect("a classified packet should parse");
        assert!(parsed.findings.is_empty(), "summary {summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), usize::from(class != Clean), "{summary:?}: {:?}", parsed.notes);
        if class != Clean {
            if let Some(severity) = severity {
                assert!(
                    notes[0].contains(&format!("declared severity: {severity}")),
                    "summary {summary:?}: {}", notes[0]
                );
            } else {
                assert!(!notes[0].contains("declared severity"), "{}", notes[0]);
            }
        }
    }
}

#[test]
fn closed_clean_templates_consume_the_whole_sentence_and_their_item_lists() {
    use DelegationSummarySentenceClass::{Clean, Unresolved};
    for (summary, class) in [
        ("No High issues found.", Clean),
        ("No High issues remain.", Clean),
        ("Found zero High issues.", Clean),
        ("Found no High issues.", Clean),
        ("Found 0 Critical / 0 High issues.", Clean),
        ("Critical 0 | High 0 | Medium 0 issues", Clean),
        ("No Low issues and no Critical issues.", Clean),
        ("Did not find one Low issue and one Critical issue.", Clean),
        ("We did not detect one Critical issue.", Clean),
        ("High-severity issue was not found.", Clean),
        ("Found and resolved one High issue and one Low issue.", Clean),
        ("Resolved one High issue and one Low issue.", Clean),
        ("Fixed one High issue found earlier.", Clean),
        ("One High-severity issue has been fixed.", Clean),
        ("Found one High-severity issue was fixed.", Clean),
        ("Found one High-severity issue, now fixed.", Clean),
        ("Found one High-severity race, fixed it.", Clean),
        ("The High-severity review is clean.", Clean),
        ("One High-severity issue is clean.", Clean),
        ("All 3906 tests passed without failures.", Clean),
        ("Ran 3 suites without errors.", Clean),
        ("No Low issues and a High-severity race.", Unresolved),
        ("No Low issues and the High-severity race found.", Unresolved),
        ("Did not find a fix for one Critical regression.", Unresolved),
        ("Could not find the root cause of one High-severity race.", Unresolved),
        ("One High-severity issue remains but the rest is clean.", Unresolved),
        ("Have not fixed the High-severity issue.", Unresolved),
        ("Previously fixed code contains a High-severity issue.", Unresolved),
        ("Found one High issue and one Low issue, now fixed.", Unresolved),
    ] {
        let sentences = delegation_result_note_sentences(summary);
        assert_eq!(sentences.len(), 1, "{summary:?}");
        let owned = delegation_result_note_words(sentences[0]);
        let words = owned.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(delegation_result_classify_summary_sentence(&words), class, "{summary:?}");
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary)).unwrap();
        assert!(parsed.findings.is_empty(), "{summary:?}");
        assert_eq!(
            inconsistency_notes(&parsed).is_empty(), class == Clean,
            "summary {summary:?}: {:?}", parsed.notes
        );
    }
    for resolution in [
        "addressed", "closed", "corrected", "eliminated", "fixed", "removed",
        "repaired", "resolved",
    ] {
        for summary in [
            format!("Found one High issue and one Low issue, now {resolution}."),
            format!("Found one High issue and one Low issue, all {resolution}."),
            format!("Found one High issue and one Low issue since {resolution}."),
        ] {
            assert_visible_unresolved_high(&summary);
        }
    }
    for negation in ["no", "none", "not", "never", "without", "zero", "0"] {
        assert_visible_unresolved_high(&format!(
            "Found one High issue and one Low issue {negation} deferred."
        ));
    }

    fn assert_visible_unresolved_high(summary: &str) {
        let sentences = delegation_result_note_sentences(summary);
        assert_eq!(sentences.len(), 1, "{summary:?}");
        let owned = delegation_result_note_words(sentences[0]);
        let words = owned.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            delegation_result_classify_summary_sentence(&words), Unresolved,
            "{summary:?}"
        );
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary)).unwrap();
        assert!(parsed.findings.is_empty(), "{summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "{summary:?}: {:?}", parsed.notes);
        assert!(
            notes[0].contains("declared severity: High"),
            "{summary:?}: {}", notes[0]
        );
    }
}

#[test]
fn unlabelled_actionable_and_severity_adjectives_keep_the_base_diagnostic() {
    for summary in [
        "Found an actionable issue.",
        "Found a critical concurrency bug.",
        "Found a high-impact race.",
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary)).unwrap();
        assert!(parsed.findings.is_empty(), "{summary:?}: {:?}", parsed.findings);
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "{summary:?}: {:?}", parsed.notes);
        assert!(!notes[0].contains("declared severity:"), "{summary:?}");
        assert_eq!(
            notes[0].as_str(),
            base_8e438fbf::contradiction_note(summary).unwrap(),
            "the unlabelled base note must be preserved for {summary:?}"
        );
    }
}

/// Frozen-base visibility, complete note text and rank over the retained corpus.
/// The snapshot below is independent of the production diagnostic helpers.
#[test]
fn base_summary_diagnostic_parity_has_only_named_exceptions() {
    let corpus = [
        "One low-severity test-quality gap was found.",
        "No issues found in the parser format, but one Low request race remains.",
        "Found one High-severity fail-open condition.",
        "Fixed one High-severity condition found during the earlier review.",
        "Reviewed 3 files and found the code clean; no high-risk areas remain.",
        "Reviewed the diff and found one module with high complexity; no defects.",
        "Found one low-severity issue; no high-risk areas remain.",
        "Found 2 (High severity).",
        "Found one issue (High).",
        "Found issues: 1 High, 1 Medium.",
        "Findings (2): both Medium.",
        "Found one issue, High severity.",
        "High throughput verified; 0 issues.",
        "Found 12 high-severity test cases, 0 issues.",
        "Found one High-severity issue.",
        "Findings: 2 Medium, 1 Low.",
        "Fixed one High-severity issue found earlier, but it remains open.",
        "Review found, after inspection, one High-severity issue.",
        "Found no Critical issues.",
        "Did not find one Critical issue.",
        "Did not find even one Critical issue.",
        "Found and resolved one Critical issue.",
        "Fixed one Critical-severity condition found earlier.",
        "Found one module with Critical complexity.",
        "No Low issues.",
        "No Critical issues.",
        "Fixed one Low-severity issue found earlier.",
        "Fixed one Critical-severity issue found earlier.",
        "Found and resolved one Low issue.",
        "Did not find one Low issue.",
        "Did not find one Low issue and one Critical issue.",
        "Found one High-severity issue, now fixed.",
        "Found one High-severity race, fixed it.",
        "Found and resolved one High-severity issue and one Low-severity issue.",
        "Found one module with Low complexity.",
        "Found one High-severity issue, possibly fixed.",
        "Found one High-severity issue and one Low-severity issue, now fixed.",
        "Fixed one Low-severity issue, found one Critical-severity issue.",
        "Fixed one Critical-severity issue, found one Low-severity issue.",
        "Did not find one Critical issue, one Low issue remains.",
        "Found not only one Low-severity issue but also one Critical-severity issue.",
        "Fixed one issue, found one High-severity issue.",
        "Fixed one Low-severity issue, found one new issue.",
        "Found one High-severity issue that the patch has not fixed.",
        "Found one High-severity issue in the dispatcher; the earlier parser race was fixed.",
        "Not ready to land: found one High-severity issue.",
        "Found without doubt one High-severity issue.",
        "Found in parser.rs one High-severity issue.",
        "Found in version 1.2 one High-severity issue.",
        "Found one High-severity issue that was never resolved.",
        "Found one High-severity issue that has not been addressed.",
        "No High issues.",
        "None of the High issues were found.",
        "Zero High issues.",
        "0 High issues.",
        "Did not find even one High-severity issue.",
        "High-severity issue not found.",
        "Fixed one High-severity issue.",
        "Resolved one High-severity issue.",
        "Addressed one High-severity issue.",
        "One High-severity issue has been fixed.",
        "One High-severity issue, now resolved.",
        "One High-severity issue, addressed it.",
        "The High-severity review is clean.",
        "A High issue awaits triage.",
        "Dispatcher manifests a Critical-severity fault.",
        "Low issue catalogued; a Critical-severity condition materializes.",
        "Fixed one High-severity issue that may have caused crashes.",
        "Have not fixed the High-severity issue.",
        "Previously fixed code contains a High-severity issue.",
        "Fixed the tests but introduced one High-severity regression.",
        "Addressed the earlier comments and one Critical race now blocks landing.",
        "Fixed X but there is one High-severity issue.",
        "One High-severity issue remains but everything else is clean.",
        "One High-severity issue remains but the rest is clean.",
        "One High-severity issue but otherwise the diff is clean.",
        "Fixed one Low issue and one Critical race now blocks landing.",
        "Did not detect one High-severity race.",
        "I did not detect one High-severity race.",
        "We did not flag one Critical regression.",
        "The review did not find one High-severity issue.",
        "The existing tests did not detect one High-severity race in the dispatcher.",
        "CI did not flag one Critical regression.",
        "No Low issues — one Critical-severity race remains.",
        "No Low issues: one Critical-severity race remains.",
        "Found 0 Critical / 2 High issues.",
        "Found 0 Critical / 0 High / 2 Medium / 1 Low findings.",
        "Critical 0 | High 1 | Medium 0 issues",
        "None of the High-severity issues were addressed.",
        "None of the three High issues has been fixed.",
        "No Low issues and one Critical-severity issue remains.",
        "No Low issues and a Critical-severity race remains.",
        "No Low issues and Critical-severity race remains.",
        "Found zero Low issues but two High-severity races remain.",
        "No Low issues – one Critical-severity race remains.",
        "Did not find one Low issue although one Critical-severity race remains.",
        "Did not find new issues and the earlier High-severity race remains open.",
        "Did not find a fix for one Critical regression.",
        "Could not find the root cause of one High-severity race.",
        "Did not find one Low issue: one Critical-severity race remains.",
        "Did not find one Low issue — one Critical-severity race remains.",
        "Did not find one Low issue / one Critical-severity race remains.",
        "Did not find one Low issue, but one Critical-severity race still blocks landing.",
        "No Critical-severity races remain.",
        "Found 0 Critical / 0 High issues.",
        "Found 0 Critical / 0 High / 0 Medium / 0 Low findings.",
        "Critical 0 | High 0 | Medium 0 issues",
        "None of the High-severity issues were found.",
        "None of the three High issues were found.",
        "No Low issues and no Critical-severity issues.",
        "Did not find even one Critical-severity race.",
        "Did not find new issues and one High-severity race.",
        "Could not find one High-severity race.",
        "Did not find one Low issue and one Critical-severity race.",
        "High-severity race was not found.",
        "No Low issues: no Critical-severity races.",
        "No Low issues — no Critical-severity races.",
        "No Low issues – no Critical-severity races.",
        "No Low issues / no Critical-severity races.",
        "Found 0 Low issues and a Critical-severity race.",
        "No Low issues and a High-severity race.",
        "No Low issues and the High-severity race found.",
        "The patch is not without a High-severity issue.",
        "Addressed none of the High-severity issues.",
        "Addressed none of the High issues.",
        "Fixed none of the three High issues.",
        "The patch fixes none of the Critical races.",
        "CI caught none of the Critical regressions.",
        "Tests cover none of the High-severity races.",
        "Fixed no High-severity issues.",
        "After upgrading to 2.0 High-severity regressions remain.",
        "Found in version 2.0 a High-severity issue.",
        "Found none of the High-severity issues.",
        "Found none of the High issues.",
        "Found none of the three High issues.",
        "Without a High-severity issue.",
        "Without High-severity issues.",
        "Found 0 Low issues and no Critical-severity race.",
        "Version 2.0 has no High issues.",
        "No High-severity issues found in the parser.",
        "CI is green; did not flag one Critical regression.",
        "CI is green, did not flag one Critical regression.",
        "CI is green: did not flag one Critical regression.",
        "Identified two Medium-severity issues.",
        "Detected one Low-severity race.",
        "FOUND ONE HIGH–SEVERITY ISSUE.",
        "A High—severity issue awaits triage.",
        "No High–severity issues.",
        "Found one High.",
        "One High.",
        "Found two High conditions.",
        "A High-severity item awaits triage.",
        "No High issues found.",
        "No High issues remain.",
        "Found zero High issues.",
        "Found no High issues.",
        "No Low issues and no Critical issues.",
        "We did not detect one Critical issue.",
        "High-severity issue was not found.",
        "Found and resolved one High issue and one Low issue.",
        "Resolved one High issue and one Low issue.",
        "Fixed one High issue found earlier.",
        "Found one High-severity issue was fixed.",
        "One High-severity issue is clean.",
        "Found one High issue and one Low issue, now fixed.",
        "Found 2 (severity: High).",
        "Found one issue (severity: High).",
        "Found 1 issue — High.",
        "Found one module with High : complexity.",
        "Found one Critical issue, one Low issue fixed.",
        "Found one Critical-severity issue and one Low-severity issue fixed.",
        "Found one Critical issue, one Low issue now fixed.",
        "One Critical issue, one Low issue removed.",
        "We found one Critical issue and one Low issue was fixed.",
        "Found one Critical issue and one Low issue, now fixed.",
        "One Critical issue and one Low issue both were fixed.",
        "One Critical issue and one Low issue, all of them are now resolved.",
        "One Critical issue was fixed.",
        "Found one Critical issue has been fixed.",
        "One Critical issue, now fixed.",
        "One Critical issue since fixed.",
        "Resolved one Critical issue and one Low issue.",
        "Found and resolved one Critical issue and one Low issue.",
        "Found one Low issue, found no Critical issues.",
        "Found one Low issue.",
        "Fixed one Critical issue, found one Low issue.",
        "Fixed one Critical issue.",
        "One Low issue remains.",
        "Found one Low-severity issue.",
        "Found one Critical-severity issue.",
        "Found one Medium-severity issue.",
        "Found one Low-severity issue and one Medium-severity issue.",
        "Found two issues: one Low-severity issue, one Medium-severity issue.",
        "Found one Low issue, and one Medium issue.",
        "Found one Low-severity issue, one Medium-severity issue.",
        "Found one Low-severity issue; one Medium-severity issue.",
        "Found one Medium-severity issue and one Low-severity issue.",
        "Found two issues: one Medium-severity issue, one Low-severity issue.",
        "Found one Medium issue, and one Low issue.",
        "Found one Medium-severity issue, one Low-severity issue.",
        "Found one Medium-severity issue; one Low-severity issue.",
        "Found one Low-severity issue and one High-severity issue.",
        "Found two issues: one Low-severity issue, one High-severity issue.",
        "Found one Low issue, and one High issue.",
        "Found one Low-severity issue, one High-severity issue.",
        "Found one Low-severity issue; one High-severity issue.",
        "Found one High-severity issue and one Low-severity issue.",
        "Found two issues: one High-severity issue, one Low-severity issue.",
        "Found one High issue, and one Low issue.",
        "Found one High-severity issue, one Low-severity issue.",
        "Found one High-severity issue; one Low-severity issue.",
        "Found one Low-severity issue and one Critical-severity issue.",
        "Found two issues: one Low-severity issue, one Critical-severity issue.",
        "Found one Low issue, and one Critical issue.",
        "Found one Low-severity issue, one Critical-severity issue.",
        "Found one Low-severity issue; one Critical-severity issue.",
        "Found one Critical-severity issue and one Low-severity issue.",
        "Found two issues: one Critical-severity issue, one Low-severity issue.",
        "Found one Critical issue, and one Low issue.",
        "Found one Critical-severity issue, one Low-severity issue.",
        "Found one Critical-severity issue; one Low-severity issue.",
        "Found one Medium-severity issue and one High-severity issue.",
        "Found two issues: one Medium-severity issue, one High-severity issue.",
        "Found one Medium issue, and one High issue.",
        "Found one Medium-severity issue, one High-severity issue.",
        "Found one Medium-severity issue; one High-severity issue.",
        "Found one High-severity issue and one Medium-severity issue.",
        "Found two issues: one High-severity issue, one Medium-severity issue.",
        "Found one High issue, and one Medium issue.",
        "Found one High-severity issue, one Medium-severity issue.",
        "Found one High-severity issue; one Medium-severity issue.",
        "Found one Medium-severity issue and one Critical-severity issue.",
        "Found two issues: one Medium-severity issue, one Critical-severity issue.",
        "Found one Medium issue, and one Critical issue.",
        "Found one Medium-severity issue, one Critical-severity issue.",
        "Found one Medium-severity issue; one Critical-severity issue.",
        "Found one Critical-severity issue and one Medium-severity issue.",
        "Found two issues: one Critical-severity issue, one Medium-severity issue.",
        "Found one Critical issue, and one Medium issue.",
        "Found one Critical-severity issue, one Medium-severity issue.",
        "Found one Critical-severity issue; one Medium-severity issue.",
        "Found one High-severity issue and one Critical-severity issue.",
        "Found two issues: one High-severity issue, one Critical-severity issue.",
        "Found one High issue, and one Critical issue.",
        "Found one High-severity issue, one Critical-severity issue.",
        "Found one High-severity issue; one Critical-severity issue.",
        "Found one Critical-severity issue and one High-severity issue.",
        "Found two issues: one Critical-severity issue, one High-severity issue.",
        "Found one Critical issue, and one High issue.",
        "Found one Critical-severity issue, one High-severity issue.",
        "Found one Critical-severity issue; one High-severity issue.",
        "Found 2 (severity : High).",
        "Found one issue (severity : High).",
        "Found 1 issue : High.",
        "Found 2 (severity - High).",
        "Found one issue (severity - High).",
        "Found 1 issue - High.",
        "Found 2 (severity — High).",
        "Found one issue (severity — High).",
        "Found 2 (severity – High).",
        "Found one issue (severity – High).",
        "Found 1 issue – High.",
        "Found 2 (severity / High).",
        "Found one issue (severity / High).",
        "Found 1 issue / High.",
        "Found 2 (severity | High).",
        "Found one issue (severity | High).",
        "Found 1 issue | High.",
        "One High-severity issue was not found.",
        "One High-severity issue not found.",
        "Found one issue (High.)",
        "Found one issue [High.]",
        "Found one issue \"High.\"",
        "Found one issue 'High.'",
        "**Found one issue: High.**",
        "_Found one issue: High._",
        "Found one issue: High...",
        "Found 2 (severity: High.)",
        "Found one issue (severity: High.)",
        "Found 1 issue — High...",
        "Found 2 (severity - High.)",
        "**Found one issue - High.**",
        "Found 1 issue - High...",
        "Found 2 (severity — High.)",
        "**Found one issue — High.**",
        "Found 2 (severity – High.)",
        "**Found one issue – High.**",
        "Found 1 issue – High...",
        "Found 2 (severity / High.)",
        "**Found one issue / High.**",
        "Found 1 issue / High...",
        "Found 2 (severity | High.)",
        "**Found one issue | High.**",
        "Found 1 issue | High...",
        "Found an actionable issue.",
        "Found a critical concurrency bug.",
        "Found a high-impact race.",
    ];
    // Each exception names the numbered CLEAN rule in the documented contract.
    let clean_exceptions = [
        ("Did not find one Critical issue", "CLEAN 2: closed reviewer negated-discovery head"),
        ("Did not find even one Critical issue", "CLEAN 2: closed reviewer negated-discovery head"),
        ("Did not find one Low issue", "CLEAN 2: closed reviewer negated-discovery head"),
        ("Did not find one Low issue and one Critical issue", "CLEAN 2: closed head distributes over complete item list"),
        ("Did not find even one High-severity issue", "CLEAN 2: closed reviewer negated-discovery head"),
        ("Did not detect one High-severity race", "CLEAN 2: closed reviewer negated-discovery head"),
        ("I did not detect one High-severity race", "CLEAN 2: explicit reviewer subject"),
        ("We did not flag one Critical regression", "CLEAN 2: explicit reviewer subject"),
        ("The review did not find one High-severity issue", "CLEAN 2: explicit reviewer subject"),
        ("None of the three High issues were found", "CLEAN 1: own negative count plus terminal were found"),
        ("Did not find even one Critical-severity race", "CLEAN 2: closed reviewer negated-discovery head"),
        ("Did not find new issues and one High-severity race", "CLEAN 2: closed head distributes over complete item list"),
        ("Could not find one High-severity race", "CLEAN 2: closed reviewer negated-discovery head"),
        ("Did not find one Low issue and one Critical-severity race", "CLEAN 2: closed head distributes over complete item list"),
        ("Found none of the three High issues", "CLEAN 1: closed discovery head and own negative count"),
        ("We did not detect one Critical issue", "CLEAN 2: explicit reviewer subject"),
        ("One High-severity issue was not found", "CLEAN 3: complete item plus was not found"),
        ("One High-severity issue not found", "CLEAN 3: complete item plus not found"),
        ("None of the High issues were found", "CLEAN 1: own negative item plus terminal were found"),
        ("0 High issues", "CLEAN 1: own zero count and complete item"),
        ("High-severity issue not found", "CLEAN 3: complete item plus not found"),
        ("None of the High-severity issues were found", "CLEAN 1: own negative item plus terminal were found"),
        ("High-severity race was not found", "CLEAN 3: complete item plus was not found"),
        ("Found none of the High-severity issues", "CLEAN 1: discovery head plus own negative item"),
        ("Found none of the High issues", "CLEAN 1: discovery head plus own negative item"),
        ("High-severity issue was not found", "CLEAN 3: complete item plus was not found"),
        ("One High-severity issue is clean", "CLEAN 7: complete item subject is clean"),
        ("Found 0 Critical / 0 High issues", "CLEAN 1: every table item owns zero"),
        ("Found 0 Critical / 0 High / 0 Medium / 0 Low findings", "CLEAN 1: every table item owns zero"),
        ("Critical 0 | High 0 | Medium 0 issues", "CLEAN 1: every inverse table item owns zero"),
        ("Found 0 Low issues and no Critical-severity race", "CLEAN 1: every item owns its negative"),
        ("Found and resolved one High-severity issue and one Low-severity issue", "CLEAN 4: discovery-and-resolution head distributes over complete list"),
        ("Found and resolved one High issue and one Low issue", "CLEAN 4: discovery-and-resolution head distributes over complete list"),
        ("Resolved one High issue and one Low issue", "CLEAN 4: resolution head distributes over complete list"),
        ("Resolved one Critical issue and one Low issue", "CLEAN 4: resolution head distributes over complete list"),
        ("Found and resolved one Critical issue and one Low issue", "CLEAN 4: discovery-and-resolution head distributes over complete list"),
    ];
    assert_eq!(corpus.len(), 297, "the retained corpus and three adjective cases");
    let mut compared = 0;
    let mut base_visible = 0;
    let mut used_exceptions = std::collections::HashSet::new();
    for summary in corpus {
        compared += 1;
        let base_note = base_8e438fbf::contradiction_note(summary);
        base_visible += usize::from(base_note.is_some());
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary)).unwrap();
        assert!(parsed.findings.is_empty(), "{summary:?}: {:?}", parsed.findings);
        let notes = inconsistency_notes(&parsed);
        assert!(notes.len() <= 1, "{summary:?}: {:?}", parsed.notes);
        let normalized = summary.trim_end_matches(['.', '!', '?']).trim();
        if let Some((_, rule)) = clean_exceptions.iter().find(|(text, _)| *text == normalized) {
            assert!(base_note.is_some(), "unused {rule} exception {summary:?}");
            assert!(
                notes.is_empty(),
                "explicit {rule} exception {summary:?}: {:?}", parsed.notes
            );
            used_exceptions.insert(normalized);
            continue;
        }
        let actual_note = notes.first().map(|note| note.as_str());
        if actual_note == base_note.as_deref() {
            continue;
        }
        let rank = |severity: &str| match severity {
            "Low" => 1, "Medium" => 2, "High" => 3, "Critical" => 4, _ => 0,
        };
        let declared = ["Low", "Medium", "High", "Critical"].into_iter()
            .find(|severity| notes.iter().any(|note| {
                note.contains(&format!("declared severity: {severity}"))
            }));
        let actual_note = actual_note.unwrap_or_else(|| {
            panic!("base visibility was lost outside a named CLEAN exception: {summary:?}")
        });
        let reason = if base_note.is_some() {
            // Named exception: HighestAttachedSeverity. C2 and the whole-sentence
            // rank contract allow only an increased label, with all text retained.
            let base_rank = base_8e438fbf::reported_severity(summary).map(rank).unwrap_or(0);
            assert!(
                declared.map(rank).unwrap_or(0) > base_rank,
                "HighestAttachedSeverity must strictly promote base rank for {summary:?}"
            );
            "HighestAttachedSeverity: C2 highest whole-sentence/attached rank replaces the base's first or absent label"
        } else {
            // Named exception: ConservativeAddedVisibility. The closed contract
            // admits unknown non-clean mentions instead of the base's local vetoes.
            "ConservativeAddedVisibility: C2 closed CLEAN and FINAL resolution contract add a non-actionable note"
        };
        if let Some(severity) = declared {
            assert!(
                summary.split(|character: char| !character.is_ascii_alphanumeric())
                    .any(|word| word.eq_ignore_ascii_case(severity)),
                "{reason} cannot invent a severity word: {summary:?}"
            );
        }
        assert_eq!(
            actual_note,
            base_8e438fbf::note_text(summary, declared),
            "{reason}; complete note wording/excerpt must match for {summary:?}"
        );
    }
    assert_eq!(compared, 297, "visibility/text parity considers every candidate");
    assert!(base_visible > 0, "the frozen visibility oracle must be nonvacuous");
    for (summary, rule) in clean_exceptions {
        assert!(
            used_exceptions.contains(summary),
            "unused exception {summary:?} ({rule}) must not hide an unexamined case"
        );
    }
}

/// Test-only frozen ForNote implementation from commit 8e438fbf, including its
/// tokenizer and count rules. Do not route this oracle through current helpers.
mod base_8e438fbf {

    pub(super) fn contradiction_note(summary: &str) -> Option<String> {
        delegation_result_summary_reports_findings(summary, Reading::ForNote)
            .then(|| note_text(summary, reported_severity(summary)))
    }

    // Frozen note wording and truncation, with the declared label supplied so
    // the test can name the contract's intentional rank/visibility differences.
    pub(super) fn note_text(summary: &str, severity: Option<&str>) -> String {
        let declared = severity
            .map(|severity| format!(" (declared severity: {severity})"))
            .unwrap_or_default();
        let compact = truncate_chars(summary, 8000);
        let excerpt = match compact.char_indices().nth(300) {
            Some((cut, _)) => format!("{}…", &compact[..cut]),
            None => compact,
        };
        format!(
            "Inconsistent result packet: the summary appears to report findings{declared}, but the Findings section says None, so no finding was recorded from the summary. Read the full output. Summary: {excerpt}"
        )
    }

    fn truncate_chars(value: &str, max_chars: usize) -> String {
        let mut end = 0;
        for (count, (index, ch)) in value.char_indices().enumerate() {
            if count == max_chars {
                return format!("{}...", &value[..end]);
            }
            end = index + ch.len_utf8();
        }
        value.to_owned()
    }

    fn delegation_result_summary_reports_findings(
        summary: &str,
        reading: Reading,
    ) -> bool {
        let (owned_tokens, clauses) = delegation_result_summary_tokens(summary, reading);
        let tokens = owned_tokens.iter().map(String::as_str).collect::<Vec<_>>();
        if delegation_result_summary_reported_severity(&tokens, &clauses, reading).is_some() {
            return true;
        }
    
        tokens.iter().enumerate().any(|(index, token)| {
            if !delegation_result_token_is_finding_noun(token) {
                return false;
            }
    
            let context_start = index.saturating_sub(4);
            let context_end = (index + 5).min(tokens.len());
            let before = &tokens[context_start..index];
            let after = &tokens[index + 1..context_end];
            if before
                .iter()
                .any(|token| matches!(*token, "no" | "not" | "zero" | "without"))
            {
                return false;
            }
    
            let context = before.iter().chain(after.iter());
            let reports_resolution = context.clone().any(|token| {
                matches!(
                    *token,
                    "addressed"
                        | "closed"
                        | "corrected"
                        | "eliminated"
                        | "fixed"
                        | "removed"
                        | "repaired"
                        | "resolved"
                )
            });
            let reports_unresolved_work = context.clone().any(|token| {
                matches!(
                    *token,
                    "open"
                        | "outstanding"
                        | "persist"
                        | "persists"
                        | "remain"
                        | "remaining"
                        | "remains"
                        | "unresolved"
                )
            });
            if reports_resolution && !reports_unresolved_work {
                return false;
            }
    
            (context_start..context_end)
                .filter(|position| *position != index)
                .any(|position| {
                    delegation_result_token_counts_findings(&tokens, &clauses, position, index, reading)
                })
                || before.iter().any(|token| {
                    matches!(
                        *token,
                        "actionable" | "critical" | "high" | "low" | "medium"
                    )
                })
        })
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Reading { AsBefore, ForNote }

    pub(super) fn reported_severity(summary: &str) -> Option<&'static str> {
        let (owned, clauses) = delegation_result_summary_tokens(summary, Reading::ForNote);
        let tokens = owned.iter().map(String::as_str).collect::<Vec<_>>();
        delegation_result_summary_reported_severity(&tokens, &clauses, Reading::ForNote)
    }

    fn delegation_result_summary_tokens(
        summary: &str,
        reading: Reading,
    ) -> (Vec<String>, Vec<usize>) {
        let mut tokens = Vec::new();
        let mut clauses = Vec::new();
        let mut clause = 0;
        let mut current = String::new();
        for character in summary.chars() {
            if character.is_ascii_alphanumeric() {
                current.push(character.to_ascii_lowercase());
                continue;
            }
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
                clauses.push(clause);
            }
            if reading == Reading::ForNote
                && matches!(character, ',' | ';' | '.' | '!' | '?')
            {
                clause += 1;
            }
        }
        if !current.is_empty() {
            tokens.push(current);
            clauses.push(clause);
        }
        (tokens, clauses)
    }

    fn delegation_result_summary_reported_severity(
        tokens: &[&str],
        clauses: &[usize],
        reading: Reading,
    ) -> Option<&'static str> {
        tokens
            .iter()
            .enumerate()
            .find_map(|(severity_index, token)| {
                let severity = delegation_result_token_severity(token)?;
                // A severity word is meaningful only when it is attached to
                // "severity" or a finding noun. This excludes incidental prose such
                // as "one module with high complexity" while retaining compact
                // declarations such as "one High-severity issue" and "one high risk".
                let previous = severity_index
                    .checked_sub(1)
                    .and_then(|index| tokens.get(index));
                let next = tokens.get(severity_index + 1);
                let is_finding_severity = previous.into_iter().chain(next).any(|token| {
                    *token == "severity" || delegation_result_token_is_finding_noun(token)
                });
                if !is_finding_severity {
                    return None;
                }
    
                let count_start = severity_index.saturating_sub(4);
                let count_end = (severity_index + 5).min(tokens.len());
                if !(count_start..count_end).any(|position| {
                    delegation_result_token_counts_findings(
                        tokens,
                        clauses,
                        position,
                        severity_index,
                        reading,
                    )
                }) {
                    return None;
                }
    
                let context_start = severity_index.saturating_sub(7);
                let context_end = (severity_index + 8).min(tokens.len());
                let context = &tokens[context_start..context_end];
                let reports_discovery = context.iter().any(|token| {
                    matches!(
                        *token,
                        "detect"
                            | "detected"
                            | "discover"
                            | "discovered"
                            | "find"
                            | "found"
                            | "flag"
                            | "flagged"
                            | "identify"
                            | "identified"
                            | "report"
                            | "reported"
                            | "uncover"
                            | "uncovered"
                    )
                });
                let reports_resolution = context.iter().any(|token| {
                    matches!(
                        *token,
                        "addressed"
                            | "closed"
                            | "corrected"
                            | "eliminated"
                            | "fixed"
                            | "removed"
                            | "repaired"
                            | "resolved"
                    )
                });
                let reports_unresolved_work = context.iter().any(|token| {
                    matches!(
                        *token,
                        "open"
                            | "outstanding"
                            | "persist"
                            | "persists"
                            | "remain"
                            | "remaining"
                            | "remains"
                            | "unresolved"
                    )
                });
    
                (reports_discovery && (!reports_resolution || reports_unresolved_work))
                    .then_some(severity)
            })
    }

    fn delegation_result_token_counts_findings(
        tokens: &[&str],
        clauses: &[usize],
        position: usize,
        anchor: usize,
        reading: Reading,
    ) -> bool {
        if !tokens
            .get(position)
            .is_some_and(|token| delegation_result_token_is_positive_count(token))
        {
            return false;
        }
        if reading == Reading::AsBefore {
            return true;
        }
        if clauses.get(position) != clauses.get(anchor) {
            return false;
        }
        // Walk back over a numeric range ("10 20" from "10-20") to its label.
        let mut label_index = position.checked_sub(1);
        while let Some(index) = label_index {
            let numeric = tokens
                .get(index)
                .is_some_and(|token| token.parse::<usize>().is_ok());
            if !numeric || clauses.get(index) != clauses.get(position) {
                break;
            }
            label_index = index.checked_sub(1);
        }
        let labels_a_step = label_index
            .and_then(|previous| tokens.get(previous))
            .is_some_and(|previous| {
                matches!(
                    *previous,
                    "attempt"
                        | "batch"
                        | "criteria"
                        | "criterion"
                        | "cut"
                        | "item"
                        | "items"
                        | "line"
                        | "lines"
                        | "part"
                        | "phase"
                        | "revision"
                        | "round"
                        | "section"
                        | "stage"
                        | "stages"
                        | "step"
                        | "steps"
                )
            });
        !labels_a_step
    }

    fn delegation_result_token_is_positive_count(token: &str) -> bool {
        matches!(
            token,
            "one" | "two" | "three" | "four" | "five" | "six" | "seven" | "eight" | "nine" | "ten"
        ) || token.parse::<usize>().is_ok_and(|count| count > 0)
    }

    fn delegation_result_token_severity(token: &str) -> Option<&'static str> {
        match token {
            "critical" => Some("Critical"),
            "high" => Some("High"),
            "medium" => Some("Medium"),
            "low" => Some("Low"),
            _ => None,
        }
    }

    fn delegation_result_token_is_finding_noun(token: &str) -> bool {
        matches!(
            token,
            "bug"
                | "bugs"
                | "defect"
                | "defects"
                | "error"
                | "errors"
                | "failure"
                | "failures"
                | "finding"
                | "findings"
                | "flaw"
                | "flaws"
                | "gap"
                | "gaps"
                | "issue"
                | "issues"
                | "problem"
                | "problems"
                | "race"
                | "races"
                | "regression"
                | "regressions"
                | "risk"
                | "risks"
        )
    }
}

#[test]
fn one_punctuation_separator_keeps_the_severity_label_attached() {
    for separator in [":", "-", "—", "–", "/", "|"] {
        for summary in [
            format!("Found 2 (severity {separator} High)."),
            format!("Found one issue (severity {separator} High)."),
            format!("Found 1 issue {separator} High."),
            format!("A High {separator} severity issue remains."),
            format!("Found one Low issue; High {separator} severity condition remains."),
            format!("Found 2 (severity {separator} High.)"),
            format!("**Found one issue {separator} High.**"),
            format!("Found 1 issue {separator} High..."),
        ] {
            let parsed = parse_delegation_result_packet(&packet_with_summary(&summary)).unwrap();
            assert!(parsed.findings.is_empty(), "{summary:?}");
            let notes = inconsistency_notes(&parsed);
            assert_eq!(notes.len(), 1, "{summary:?}: {:?}", parsed.notes);
            assert!(
                notes[0].contains("declared severity: High"),
                "{summary:?}: {}", notes[0]
            );
        }
    }
    for summary in [
        "Found 2 (severity: High).",
        "Found one issue (severity: High).",
        "Found 1 issue — High.",
        "Found one issue (High.)",
        "Found one issue [High.]",
        "Found one issue \"High.\"",
        "Found one issue 'High.'",
        "**Found one issue: High.**",
        "_Found one issue: High._",
        "Found one issue: High...",
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary)).unwrap();
        assert!(parsed.findings.is_empty(), "{summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "{summary:?}: {:?}", parsed.notes);
        assert!(notes[0].contains("declared severity: High"), "{summary:?}");
    }
    let parsed = parse_delegation_result_packet(&packet_with_summary(
        "Found one module with High : complexity."
    )).unwrap();
    assert!(parsed.findings.is_empty());
    assert!(inconsistency_notes(&parsed).is_empty());
    for (text, expected) in [
        ("parser.rs", vec!["parser.rs"]),
        ("version 2.0", vec!["version", "2.0"]),
        ("High.)", vec!["high"]),
        ("High...", vec!["high"]),
    ] {
        assert_eq!(delegation_result_note_words(text), expected, "{text:?}");
    }
}

#[test]
fn trailing_resolution_requires_one_complete_item_and_an_affirmative_marker() {
    use DelegationSummarySentenceClass::{Clean, Unresolved};
    for (summary, expected) in [
        ("Found one Critical issue, one Low issue fixed.", Some("Critical")),
        ("Found one Critical-severity issue and one Low-severity issue fixed.", Some("Critical")),
        ("Found one Critical issue, one Low issue now fixed.", Some("Critical")),
        ("One Critical issue, one Low issue removed.", Some("Critical")),
        ("We found one Critical issue and one Low issue was fixed.", Some("Critical")),
        ("Found one Critical issue and one Low issue, now fixed.", Some("Critical")),
        ("One Critical issue and one Low issue both were fixed.", Some("Critical")),
        ("One Critical issue and one Low issue, all of them are now resolved.", Some("Critical")),
        ("One Critical issue was fixed.", None),
        ("Found one Critical issue has been fixed.", None),
        ("One Critical issue, now fixed.", None),
        ("One Critical issue since fixed.", None),
        ("Resolved one Critical issue and one Low issue.", None),
        ("Found and resolved one Critical issue and one Low issue.", None),
    ] {
        check(summary, expected);
    }
    for resolution in [
        "addressed", "closed", "corrected", "eliminated", "fixed", "removed",
        "repaired", "resolved",
    ] {
        check(&format!("One Critical issue was {resolution}."), None);
        check(&format!("One Critical issue {resolution}."), Some("Critical"));
        check(
            &format!("Found one Critical issue and one Low issue was {resolution}."),
            Some("Critical"),
        );
    }

    fn check(summary: &str, expected: Option<&str>) {
        let sentences = delegation_result_note_sentences(summary);
        assert_eq!(sentences.len(), 1, "{summary:?}");
        let owned = delegation_result_note_words(sentences[0]);
        let words = owned.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            delegation_result_classify_summary_sentence(&words),
            if expected.is_some() { Unresolved } else { Clean },
            "{summary:?}"
        );
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary)).unwrap();
        assert!(parsed.findings.is_empty(), "{summary:?}");
        let notes = inconsistency_notes(&parsed);
        if let Some(severity) = expected {
            assert_eq!(notes.len(), 1, "{summary:?}: {:?}", parsed.notes);
            assert!(
                notes[0].contains(&format!("declared severity: {severity}")),
                "{summary:?}: {}", notes[0]
            );
        } else {
            assert!(notes.is_empty(), "{summary:?}: {:?}", parsed.notes);
        }
    }
}

#[test]
fn sentence_separation_controls_the_intentional_mixed_severity_diagnostic() {
    for (summary, severity) in [
        ("Found one Low issue, found no Critical issues.", "Critical"),
        ("Found one Low issue. Found no Critical issues.", "Low"),
        ("Fixed one Critical issue, found one Low issue.", "Critical"),
        ("Fixed one Critical issue. Found one Low issue.", "Low"),
        ("Did not find one Critical issue, one Low issue remains.", "Critical"),
        ("Did not find one Critical issue. One Low issue remains.", "Low"),
    ] {
        let parsed = parse_delegation_result_packet(&packet_with_summary(summary)).unwrap();
        assert!(parsed.findings.is_empty(), "{summary:?}");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), 1, "{summary:?}");
        assert!(
            notes[0].contains(&format!("declared severity: {severity}")),
            "summary {summary:?}: {}", notes[0]
        );
    }
}

#[test]
fn mixed_summary_severities_do_not_relabel_explicit_or_recovered_findings() {
    let summary = "Found one Low-severity issue. Found one Critical-severity issue.";
    let finding = "- **[High]** `src/parser.rs:7` - Preserve the structured finding.";
    let explicit = format!(
        "## Result\n\nStatus: completed\n\nSummary:\n{summary}\n\nFindings:\n{finding}"
    );
    let recovered = format!(
        "# Code Review\n\n## Actionable\n{finding}\n\n{}",
        packet_with_summary(summary)
    );
    for (packet, contradiction) in [(explicit, false), (recovered, true)] {
        let parsed = parse_delegation_result_packet(&packet).expect("the packet should parse");
        assert_eq!(parsed.findings.len(), 1);
        let finding = &parsed.findings[0];
        assert_eq!(finding.severity, "High");
        assert_eq!(finding.file.as_deref(), Some("src/parser.rs"));
        assert_eq!(finding.line, Some(7));
        assert_eq!(finding.message, "Preserve the structured finding.");
        let notes = inconsistency_notes(&parsed);
        assert_eq!(notes.len(), usize::from(contradiction));
        if contradiction {
            assert!(notes[0].contains("declared severity: Critical"));
        }
    }
}

/// The integration review's examples: a summary that states its count apart
/// from the finding word, by a colon, a parenthesis or a wrapped line, still
/// contradicts an explicit None, so the structured findings the reviewer wrote
/// in the preamble are recovered as before and the note names the
/// contradiction. Never a clean packet.
#[test]
fn a_count_beside_a_colon_parenthesis_or_wrap_still_recovers_the_preamble_findings() {
    let mut failures = Vec::new();
    for summary in [
        "Issues found: 2.",
        "Findings: 2 Medium, 1 Low.",
        "Found 2 (High severity).",
        "Found two\nissues in the parser.",
    ] {
        let packet = format!(
            "# Code Review\n\n\
## Actionable\n\
- **[Medium]** `src/parser.rs:7` \u{2014} A parser regression remains.\n\n\
## Result\n\n\
Status: completed\n\n\
Summary:\n\
{summary}\n\n\
Findings:\n\
- None"
        );
        let parsed = parse_delegation_result_packet(&packet).expect("the packet should parse");
        let recovered = parsed.findings.len() == 1
            && parsed.findings[0].severity == "Medium"
            && parsed.findings[0].file.as_deref() == Some("src/parser.rs");
        let noted = inconsistency_notes(&parsed).len() == 1;
        if !(recovered && noted) {
            failures.push(format!(
                "{summary:?}: findings {:?}, notes {:?}",
                parsed.findings, parsed.notes
            ));
        }
    }
    assert!(failures.is_empty(), "clean packets:\n{}", failures.join("\n"));
}

/// The note repeats only the start of a long summary, ending in an ellipsis,
/// and points to the full output for the rest.
#[test]
fn the_inconsistency_note_repeats_only_the_start_of_a_long_summary() {
    let summary = format!("Found one High-severity issue. {}", "Context words. ".repeat(200));
    let parsed = parse_delegation_result_packet(&packet_with_summary(&summary))
        .expect("a contradictory packet should parse");

    let notes = inconsistency_notes(&parsed);
    assert_eq!(notes.len(), 1, "{:?}", parsed.notes);
    let (_, excerpt) = notes[0]
        .split_once("Summary: ")
        .expect("the note should repeat the summary");
    assert!(excerpt.starts_with("Found one High-severity issue."), "{excerpt}");
    assert!(excerpt.ends_with('\u{2026}'), "{excerpt}");
    assert_eq!(excerpt.chars().count(), 301, "{excerpt}");
}

