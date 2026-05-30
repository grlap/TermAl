use super::*;

#[test]
fn delegation_result_packet_accepts_preamble_and_case_drift() {
    let parsed = parse_delegation_result_packet(
        "Done, here is the packet:\n\n## result\n\nstatus: completed\n\nsummary:\nReady.",
    )
    .expect("packet with preamble and lowercase labels should parse");
    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(parsed.summary, "Ready.");

    let parsed = parse_delegation_result_packet("## RESULT\n\nSTATUS: failed\n\nSummary:\nNope.")
        .expect("uppercase status label should parse");
    assert_eq!(parsed.status, DelegationStatus::Failed);
    assert_eq!(parsed.summary, "Nope.");
}

#[test]
fn delegation_result_packet_summary_allows_colon_terminated_text_lines() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nThe issue is here:\n  detail\n\nNotes:\nignored",
    )
    .expect("summary text ending in colon should not terminate the summary");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(parsed.summary, "The issue is here:\n  detail");
}

#[test]
fn delegation_result_packet_summary_preserves_status_labeled_text() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nStatus: the inspected path is stable.\nNo changes needed.",
    )
    .expect("summary text containing Status: should not reset packet metadata");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(
        parsed.summary,
        "Status: the inspected path is stable.\nNo changes needed."
    );
}

#[test]
fn delegation_result_packet_parses_findings_notes_and_inspected_files() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- High src/delegations.rs:1413 - Resume prompt drops findings.\n- Note docs/features/agent-delegation-sessions.md - Document the fan-in path.\n\nNotes:\n- Checked backend wait dispatch.\n\nFiles Inspected:\n- src/delegations.rs\n- ui/src/delegation-commands.ts",
    )
    .expect("packet with findings and notes should parse");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(parsed.summary, "Ready.");
    assert_eq!(
        parsed.findings,
        vec![
            DelegationFinding {
                severity: "High".to_owned(),
                file: Some("src/delegations.rs".to_owned()),
                line: Some(1413),
                message: "Resume prompt drops findings.".to_owned(),
            },
            DelegationFinding {
                severity: "Note".to_owned(),
                file: Some("docs/features/agent-delegation-sessions.md".to_owned()),
                line: None,
                message: "Document the fan-in path.".to_owned(),
            },
        ]
    );
    assert_eq!(
        parsed.notes,
        vec![
            "Checked backend wait dispatch.".to_owned(),
            "Inspected src/delegations.rs".to_owned(),
            "Inspected ui/src/delegation-commands.ts".to_owned(),
        ]
    );
}

#[test]
fn delegation_result_packet_deduplicates_and_caps_findings() {
    let mut packet = "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n\
- High src/delegations.rs:1413 - Resume prompt drops findings.\n\
- High src/delegations.rs:1413 - Resume prompt drops findings.\n"
        .to_owned();
    for index in 0..MAX_DELEGATION_RESULT_FINDINGS {
        packet.push_str(&format!(
            "- Low src/generated.rs:{index} - Generated finding {index}.\n"
        ));
    }

    let parsed = parse_delegation_result_packet(&packet).expect("packet findings should parse");

    assert_eq!(parsed.findings.len(), MAX_DELEGATION_RESULT_FINDINGS);
    assert_eq!(
        parsed.findings[0],
        DelegationFinding {
            severity: "High".to_owned(),
            file: Some("src/delegations.rs".to_owned()),
            line: Some(1413),
            message: "Resume prompt drops findings.".to_owned(),
        }
    );
    assert_eq!(
        parsed
            .findings
            .iter()
            .filter(|finding| finding.message == "Resume prompt drops findings.")
            .count(),
        1
    );
    assert!(
        parsed
            .findings
            .iter()
            .all(|finding| finding.message != "Generated finding 99."),
        "cap should be applied after dedupe keeps the first unique finding"
    );
}

#[test]
fn delegation_result_packet_parses_line_range_on_standard_findings_path() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Medium src/state.rs:66-109 - State mutex waits behind the mailbox.",
    )
    .expect("packet with line-range finding should parse");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Medium".to_owned(),
            file: Some("src/state.rs".to_owned()),
            line: Some(66),
            message: "State mutex waits behind the mailbox.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_recovers_actionable_findings_when_final_packet_defers() {
    let parsed = parse_delegation_result_packet(
        "# Code Review\n\n\
## Actionable\n\
- **[Medium]** `src/state.rs:66-109` \u{2014} State mutex waits behind the bounded mailbox.\n\
- **[Low]** [docs/bugs.md](/Users/greg/GitHub/Personal/termal/docs/bugs.md:217) \u{2014} Task wording drifted.\n\
  - Why it matters: parent fan-in needs concrete findings.\n\n\
## Informational\n\
- No other issues found.\n\n\
## Result\n\n\
Status: completed\n\n\
Findings:\n\
- Note - See the Actionable and Informational sections above; the headline finding is the state-mutex coupling.\n\n\
Files Inspected:\n\
- src/state.rs",
    )
    .expect("deferential packet should recover structured actionable findings");

    assert_eq!(
        parsed.findings,
        vec![
            DelegationFinding {
                severity: "Medium".to_owned(),
                file: Some("src/state.rs".to_owned()),
                line: Some(66),
                message: "State mutex waits behind the bounded mailbox.".to_owned(),
            },
            DelegationFinding {
                severity: "Low".to_owned(),
                file: Some("docs/bugs.md".to_owned()),
                line: Some(217),
                message: "Task wording drifted.".to_owned(),
            },
        ]
    );
    assert_eq!(parsed.notes, vec!["Inspected src/state.rs".to_owned()]);
}

#[test]
fn delegation_result_packet_recovers_alternative_actionable_finding_shapes() {
    let parsed = parse_delegation_result_packet(
        "# Code Review\n\n\
## Actionable\n\
- [High] `src/state.rs:66-109` - Bracket severity with a regular hyphen.\n\
- **Low** [docs/bugs.md](/Users/greg/GitHub/Personal/termal/docs/bugs.md:217) \u{2013} Bold severity with an en dash.\n\
- **[Note]** src/delegations.rs:42 \u{2014} Bracketed bold severity with an em dash.\n\n\
## Result\n\n\
Status: completed\n\n\
Findings:\n\
- Note - See the Actionable section above.",
    )
    .expect("deferential packet should recover alternative actionable finding shapes");

    assert_eq!(
        parsed.findings,
        vec![
            DelegationFinding {
                severity: "High".to_owned(),
                file: Some("src/state.rs".to_owned()),
                line: Some(66),
                message: "Bracket severity with a regular hyphen.".to_owned(),
            },
            DelegationFinding {
                severity: "Low".to_owned(),
                file: Some("docs/bugs.md".to_owned()),
                line: Some(217),
                message: "Bold severity with an en dash.".to_owned(),
            },
            DelegationFinding {
                severity: "Note".to_owned(),
                file: Some("src/delegations.rs".to_owned()),
                line: Some(42),
                message: "Bracketed bold severity with an em dash.".to_owned(),
            },
        ]
    );
}

#[test]
fn delegation_result_packet_rejects_deferential_findings_without_actionable_preamble() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\n\
Status: completed\n\n\
Findings:\n\
- Note - See the Actionable and Informational sections above; the headline finding is elsewhere.",
    );

    assert!(
        parsed.is_none(),
        "deferential findings without a parseable Actionable section should not complete"
    );
}

#[test]
fn delegation_result_packet_explicit_none_does_not_recover_actionable_preamble() {
    let parsed = parse_delegation_result_packet(
        "# Code Review\n\n\
## Actionable\n\
- **[Medium]** `src/state.rs:66-109` \u{2014} Stale preamble finding.\n\n\
## Result\n\n\
Status: completed\n\n\
Summary:\n\
Reviewed again and found no issues.\n\n\
Findings:\n\
- None",
    )
    .expect("explicit no-findings result should parse");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(parsed.summary, "Reviewed again and found no issues.");
    assert!(parsed.findings.is_empty());
}

#[test]
fn delegation_result_packet_drops_trailing_colon_from_invalid_finding_line() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Low src/foo.rs: - Missing line number.",
    )
    .expect("packet should parse finding with invalid line suffix");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Low".to_owned(),
            file: Some("src/foo.rs".to_owned()),
            line: None,
            message: "Missing line number.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_filters_none_findings() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- None",
    )
    .expect("packet with explicit no-findings marker should parse");

    assert!(parsed.findings.is_empty());
}

#[test]
fn delegation_result_packet_no_separator_finding_uses_note_fallback() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Missing separator but still useful.",
    )
    .expect("packet with fallback finding should parse");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Note".to_owned(),
            file: None,
            line: None,
            message: "Missing separator but still useful.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_parses_multi_word_finding_severity() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Code Style src/foo.rs:42 - Use repo formatting.",
    )
    .expect("packet with multi-word finding severity should parse");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Code Style".to_owned(),
            file: Some("src/foo.rs".to_owned()),
            line: Some(42),
            message: "Use repo formatting.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_parses_multi_word_severity_with_backticked_location() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Code Style `src/foo.rs:42` - Use repo formatting.",
    )
    .expect("packet with backticked multi-word finding location should parse");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Code Style".to_owned(),
            file: Some("src/foo.rs".to_owned()),
            line: Some(42),
            message: "Use repo formatting.".to_owned(),
        }]
    );
}
