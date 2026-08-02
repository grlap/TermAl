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
fn delegation_result_packet_accepts_inline_summary_and_nested_review_sections() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\n\
Status: completed\n\n\
Summary: Reviewed the current changes through every configured lens.\n\n\
# Code Review\n\n\
## Findings\n\n\
### Actionable\n\n\
#### Medium\n\n\
- **[Medium]** [src/state.rs:108](/repo/src/state.rs:108) — Lock diagnostics must not block the mutex owner.\n\n\
### Informational\n\n\
- **[High]** `ignored.rs:1` — This is explicitly non-actionable.\n\n\
## Commands Run\n\n\
- `git diff`",
    )
    .expect("semantic packet sections should parse independently of heading depth");

    assert_eq!(
        parsed.summary,
        "Reviewed the current changes through every configured lens."
    );
    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Medium".to_owned(),
            file: Some("src/state.rs".to_owned()),
            line: Some(108),
            message: "Lock diagnostics must not block the mutex owner.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_keeps_the_first_summary_section_canonical() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\n\
Status: completed\n\n\
Summary: Canonical parent-card summary.\n\n\
## Findings\n\n\
### Actionable\n\n\
- **[Low]** `src/state.rs:1` — Real finding.\n\n\
## Summary\n\n\
This later review summary must not be appended to the packet summary.\n\n\
## Commands Run\n\n\
- `git diff`",
    )
    .expect("repeated summary headings should not corrupt the canonical summary");

    assert_eq!(parsed.summary, "Canonical parent-card summary.");
    assert_eq!(parsed.findings.len(), 1);
    assert_eq!(parsed.findings[0].severity, "Low");
}

#[test]
fn delegation_review_findings_reject_nonseverity_links_and_unrelated_tables() {
    let findings = parse_delegation_review_findings(
        "## Findings\n\n\
### Actionable\n\n\
- **[docs/guide.md](https://example.test/guide)** — Documentation changed.\n\n\
| File | Measurement | Result |\n\
|---|---|---|\n\
| src/state.rs | 42ms | stable |\n\n\
- **[Low]** `src/state.rs:108` — Real finding.",
    );

    assert_eq!(
        findings,
        vec![DelegationFinding {
            severity: "Low".to_owned(),
            file: Some("src/state.rs".to_owned()),
            line: Some(108),
            message: "Real finding.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_parses_markdown_headed_table_review() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\n\
Status: completed\n\n\
### Summary\n\n\
Reviewed 26 files and found three actionable issues.\n\n\
### Findings: Code Review — 2026-08-02\n\n\
#### Changes Reviewed\n\n\
- 26 unstaged files.\n\n\
#### Actionable\n\n\
| Severity | Location | Finding |\n\
|---|---|---|\n\
| High | [data-inventory-schema.json:288](/repo/data-inventory-schema.json:288) | Coverage metadata is not verified. |\n\
| Medium | [attestation_docket.py:556](/repo/attestation_docket.py:556) | Docket trust differs from the engine. |\n\n\
#### Informational\n\n\
- This informational bullet is not actionable.\n\n\
### Commands Run\n\n\
- `git diff --check`",
    )
    .expect("Markdown-headed table result should parse");

    assert_eq!(
        parsed.summary,
        "Reviewed 26 files and found three actionable issues."
    );
    assert_eq!(
        parsed.findings,
        vec![
            DelegationFinding {
                severity: "High".to_owned(),
                file: Some("data-inventory-schema.json".to_owned()),
                line: Some(288),
                message: "Coverage metadata is not verified.".to_owned(),
            },
            DelegationFinding {
                severity: "Medium".to_owned(),
                file: Some("attestation_docket.py".to_owned()),
                line: Some(556),
                message: "Docket trust differs from the engine.".to_owned(),
            },
        ]
    );
}

#[test]
fn delegation_result_packet_parses_markdown_headed_bullet_review() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\n\
Status: completed\n\n\
## Summary:\n\
Reviewed the legal conformance changes.\n\n\
## Findings:\n\n\
### Actionable\n\n\
- **[MEDIUM] M1 — committed site embeds a stale test count** — `legal/site/index.html` embeds 3262 tests.\n\
- **[LOW]** `legal/tooling/conformance/attestation_docket.py:556` — Fact and record identifiers can diverge.\n\n\
### Informational\n\n\
- **[HIGH]** `ignored.rs:1` — Informational text must not become a finding.\n\n\
## Files Inspected:\n\
- legal/site/index.html",
    )
    .expect("Markdown-headed bullet result should parse");

    assert_eq!(parsed.summary, "Reviewed the legal conformance changes.");
    assert_eq!(parsed.findings.len(), 2);
    assert_eq!(parsed.findings[0].severity, "MEDIUM");
    assert_eq!(
        parsed.findings[0].file.as_deref(),
        Some("legal/site/index.html")
    );
    assert!(
        parsed.findings[0]
            .message
            .contains("committed site embeds a stale test count")
    );
    assert_eq!(parsed.findings[1].severity, "LOW");
    assert_eq!(
        parsed.findings[1].file.as_deref(),
        Some("legal/tooling/conformance/attestation_docket.py")
    );
    assert_eq!(parsed.findings[1].line, Some(556));
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
