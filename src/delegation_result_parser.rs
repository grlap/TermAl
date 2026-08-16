// Owns semantic parsing and degraded-result synthesis for delegated reviewer
// result packets.
// Does not own delegation lifecycle, persistence, repair admission, or parent
// card updates.
// Split from src/delegations.rs so packet-format tolerance can evolve without
// growing the delegation coordinator further.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DelegationResultSection {
    Summary,
    Findings,
    Notes,
    FilesInspected,
    Ignored,
}

fn parse_delegation_result_packet(text: &str) -> Option<ParsedDelegationResult> {
    let search_window = delegation_result_search_window(text);
    let result_marker_start = delegation_result_marker_start(search_window)?;
    let preamble = &search_window[..result_marker_start];
    let mut lines = search_window[result_marker_start..].lines();
    lines.next()?;

    let mut status = None;
    let mut summary_lines = Vec::new();
    let mut finding_lines = Vec::new();
    let mut note_lines: Vec<String> = Vec::new();
    let mut section: Option<DelegationResultSection> = None;
    let mut saw_summary_section = false;
    for line in lines {
        let cleaned = line.trim();
        if section.is_none() {
            if let Some((label, value)) = cleaned.split_once(':') {
                match label.trim().to_ascii_lowercase().as_str() {
                    "status" => {
                        status = match value.trim().to_ascii_lowercase().as_str() {
                            "completed" => Some(DelegationStatus::Completed),
                            "failed" => Some(DelegationStatus::Failed),
                            _ => None,
                        };
                        continue;
                    }
                    "summary" => {
                        saw_summary_section = true;
                        section = Some(DelegationResultSection::Summary);
                        if !value.trim().is_empty() {
                            summary_lines.push(value.trim());
                        }
                        continue;
                    }
                    _ => {}
                }
            }
        }

        if let Some(next_section) = delegation_result_section_heading(cleaned) {
            let nested_code_review = section == Some(DelegationResultSection::Findings)
                && next_section == DelegationResultSection::Ignored
                && delegation_review_section_label(cleaned)
                    .is_some_and(|label| is_decorated_code_review_heading(&label));
            if nested_code_review {
                // Some reviewers place their entire Markdown review directly
                // under the packet's `Findings:` label. Keep the review title
                // inside the captured payload so a later `## Actionable`
                // heading can reopen actionable finding capture.
                finding_lines.push(line);
                continue;
            }
            if next_section == DelegationResultSection::Summary {
                if saw_summary_section {
                    // The first Summary is the packet's canonical summary.
                    // Nested review prose can contain another Summary heading;
                    // reopening capture there concatenates unrelated text into
                    // the compact parent-card result.
                    section = Some(DelegationResultSection::Ignored);
                    continue;
                }
                saw_summary_section = true;
            }
            section = Some(next_section);
            continue;
        }
        if cleaned.starts_with('#') {
            match section {
                Some(DelegationResultSection::Summary) => summary_lines.push(line),
                Some(DelegationResultSection::Findings) => finding_lines.push(line),
                Some(DelegationResultSection::Notes) => note_lines.push(line.to_owned()),
                Some(DelegationResultSection::FilesInspected)
                | Some(DelegationResultSection::Ignored)
                | None => {}
            }
            continue;
        }

        match section {
            Some(DelegationResultSection::Summary) => summary_lines.push(line),
            Some(DelegationResultSection::Findings) => finding_lines.push(line),
            Some(DelegationResultSection::Notes) => note_lines.push(line.to_owned()),
            Some(DelegationResultSection::FilesInspected) => {
                if let Some(note) = parse_delegation_note_line(line) {
                    note_lines.push(format!("Inspected {note}"));
                }
            }
            Some(DelegationResultSection::Ignored) | None => {}
        }
    }

    let status = status?;
    let summary = summary_lines.join("\n").trim().to_owned();
    let summary = if summary.is_empty() {
        match status {
            DelegationStatus::Completed => "Delegation completed.".to_owned(),
            DelegationStatus::Failed => "Delegation failed.".to_owned(),
            _ => String::new(),
        }
    } else {
        summary
    };

    // Reviewer agents commonly keep their full Markdown review inside the
    // packet's Findings section (Actionable/Informational subsections, tables,
    // bold severities, and so on). Prefer that structured review parser before
    // falling back to the compact one-line packet format.
    let findings_review = format!("## Findings\n{}", finding_lines.join("\n"));
    let mut findings = parse_delegation_review_findings_impl(&findings_review, true);
    let findings_explicitly_empty = delegation_result_findings_explicitly_empty(&finding_lines);
    let findings_refer_to_prior_sections = findings
        .iter()
        .any(delegation_finding_refers_to_prior_sections)
        || delegation_finding_lines_refer_to_prior_sections(&finding_lines);
    findings.retain(|finding| !delegation_finding_refers_to_prior_sections(finding));
    // An explicit empty Findings section is authoritative unless the summary
    // positively claims one or more findings. This avoids resurrecting stale
    // preamble prose merely because a clean reviewer used wording outside a
    // brittle allow-list, while still repairing the observed packet shape
    // whose summary says that findings exist but whose compact list says None.
    let contradictory_empty_findings =
        findings_explicitly_empty && delegation_result_summary_reports_findings(&summary);
    if (!findings_explicitly_empty || contradictory_empty_findings)
        && (findings.is_empty() || findings_refer_to_prior_sections)
    {
        let preamble_findings = parse_delegation_review_findings(preamble);
        if !preamble_findings.is_empty() {
            let mut merged_findings = preamble_findings;
            merged_findings.extend(findings);
            findings = dedupe_delegation_findings(merged_findings);
        } else if contradictory_empty_findings {
            // Never turn a self-contradictory completed review into a clean
            // result. If the reviewer omitted parseable details entirely,
            // preserve the declared severity and direct the parent to the
            // authoritative full output.
            findings.push(delegation_result_summary_fallback_finding(&summary));
        } else if findings_refer_to_prior_sections && findings.is_empty() {
            return None;
        }
    }

    Some(ParsedDelegationResult {
        status,
        summary: compact_delegation_result_summary(&summary),
        findings: dedupe_delegation_findings(findings),
        notes: note_lines
            .iter()
            .filter_map(|line| parse_delegation_note_line(line))
            .collect(),
    })
}

fn delegation_result_summary_reports_findings(summary: &str) -> bool {
    let normalized = summary
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>();
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if delegation_result_summary_reported_severity(&tokens).is_some() {
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

        before
            .iter()
            .chain(after.iter())
            .any(|token| delegation_result_token_is_positive_count(token))
            || before.iter().any(|token| {
                matches!(
                    *token,
                    "actionable" | "critical" | "high" | "low" | "medium"
                )
            })
    })
}

fn delegation_result_summary_reported_severity(tokens: &[&str]) -> Option<&'static str> {
    tokens.iter().enumerate().find_map(|(severity_index, token)| {
        let severity = delegation_result_token_severity(token)?;
        // A severity word is meaningful only when it is attached to
        // "severity" or a finding noun. This excludes incidental prose such
        // as "one module with high complexity" while retaining compact
        // declarations such as "one High-severity issue" and "one high risk".
        let previous = severity_index.checked_sub(1).and_then(|index| tokens.get(index));
        let next = tokens.get(severity_index + 1);
        let is_finding_severity = previous
            .into_iter()
            .chain(next)
            .any(|token| *token == "severity" || delegation_result_token_is_finding_noun(token));
        if !is_finding_severity {
            return None;
        }

        let count_start = severity_index.saturating_sub(4);
        let count_end = (severity_index + 5).min(tokens.len());
        if !tokens[count_start..count_end]
            .iter()
            .any(|token| delegation_result_token_is_positive_count(token))
        {
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

        (reports_discovery && (!reports_resolution || reports_unresolved_work)).then_some(severity)
    })
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

fn delegation_result_summary_fallback_finding(summary: &str) -> DelegationFinding {
    let normalized = summary
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>();
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let severity = delegation_result_summary_reported_severity(&tokens).unwrap_or("Unspecified");
    DelegationFinding {
        severity: severity.to_owned(),
        file: None,
        line: None,
        message: format!(
            "Reviewer summary reports an actionable finding, but the result packet omitted its structured details. Inspect the full reviewer output. Summary: {}",
            compact_delegation_result_summary(summary)
        ),
    }
}

fn delegation_result_token_is_positive_count(token: &str) -> bool {
    matches!(
        token,
        "one" | "two" | "three" | "four" | "five" | "six" | "seven" | "eight" | "nine" | "ten"
    ) || token.parse::<usize>().is_ok_and(|count| count > 0)
}

fn synthesize_delegation_result_from_assistant_output(
    text: &str,
) -> Option<ParsedDelegationResult> {
    let summary = non_empty_trimmed(text)?;
    if is_delegation_stop_marker(&summary) {
        return None;
    }
    // In the plain-output fallback, an idle child that produced any non-stop
    // assistant text completed its turn. Preserve that output as a completed
    // degraded result even when the text describes a soft failure.
    Some(ParsedDelegationResult {
        status: DelegationStatus::Completed,
        findings: parse_delegation_review_findings(&summary),
        summary: compact_delegation_result_summary(&summary),
        notes: Vec::new(),
    })
}

fn is_delegation_stop_marker(text: &str) -> bool {
    text.trim() == SESSION_STOPPED_BY_USER_MESSAGE
}

fn delegation_result_section_heading(cleaned: &str) -> Option<DelegationResultSection> {
    // Accept both the compact packet contract (`Summary:`) and the Markdown
    // headings reviewers actually emit (`### Summary`, `## Findings:`, or
    // `### Findings: Code Review — 2026-08-02`). A prose line ending in a
    // colon is still only a section when its complete label is known.
    let is_markdown_heading = cleaned.starts_with('#');
    if !is_markdown_heading && !cleaned.ends_with(':') {
        return None;
    }
    let label = delegation_review_section_label(cleaned)?;
    let section_label = label
        .split_once(':')
        .map(|(prefix, _)| prefix.trim())
        .unwrap_or(label.as_str());
    match section_label {
        "summary" => Some(DelegationResultSection::Summary),
        "findings" => Some(DelegationResultSection::Findings),
        "notes" => Some(DelegationResultSection::Notes),
        "files inspected" => Some(DelegationResultSection::FilesInspected),
        "commands run" => Some(DelegationResultSection::Ignored),
        _ if is_decorated_code_review_heading(section_label) => {
            Some(DelegationResultSection::Ignored)
        }
        _ => None,
    }
}

fn is_decorated_code_review_heading(label: &str) -> bool {
    let Some(suffix) = label.strip_prefix("code review") else {
        return false;
    };
    let suffix = suffix.trim_start();
    suffix.is_empty() || matches!(suffix.chars().next(), Some(':' | '-' | '\u{2013}' | '\u{2014}'))
}

fn parse_delegation_note_line(line: &str) -> Option<String> {
    let text = normalize_delegation_result_list_item(line);
    if text.is_empty() || is_delegation_no_findings_marker(text) {
        return None;
    }
    Some(text.to_owned())
}

fn parse_delegation_finding_line(line: &str) -> Option<DelegationFinding> {
    let text = normalize_delegation_result_list_item(line);
    if text.is_empty() || is_delegation_no_findings_marker(text) {
        return None;
    }
    let (head, message) = text.split_once(" - ")?;
    let head = head.trim();
    let message = message.trim();
    if head.is_empty() || message.is_empty() {
        return None;
    }
    let (severity, location) = parse_delegation_finding_head(head);
    if !is_delegation_review_severity(severity) {
        return None;
    }
    let (file, line) = parse_delegation_finding_location(location);
    Some(DelegationFinding {
        severity: severity.to_owned(),
        file,
        line,
        message: message.to_owned(),
    })
}

fn delegation_result_findings_explicitly_empty(lines: &[&str]) -> bool {
    let mut saw_empty_marker = false;
    for line in lines {
        let text = normalize_delegation_result_list_item(line);
        if text.is_empty() {
            continue;
        }
        if !is_delegation_no_findings_marker(text) {
            return false;
        }
        saw_empty_marker = true;
    }
    saw_empty_marker
}

fn delegation_finding_refers_to_prior_sections(finding: &DelegationFinding) -> bool {
    delegation_message_is_prior_section_reference(&finding.message)
}

fn delegation_finding_lines_refer_to_prior_sections(lines: &[&str]) -> bool {
    lines.iter().any(|line| {
        let text = normalize_delegation_result_list_item(line);
        let message = text
            .split_once(" - ")
            .map(|(_, message)| message)
            .unwrap_or(text);
        delegation_message_is_prior_section_reference(message)
    })
}

fn delegation_message_is_prior_section_reference(message: &str) -> bool {
    let message = message.trim().to_ascii_lowercase();
    [
        "see the actionable",
        "see actionable",
        "see the informational",
        "see informational",
        "see the sections above",
        "see sections above",
        "actionable and informational sections above",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
}

fn parse_delegation_review_findings(text: &str) -> Vec<DelegationFinding> {
    parse_delegation_review_findings_impl(text, false)
}

fn parse_delegation_review_findings_impl(
    text: &str,
    allow_compact_packet_lines: bool,
) -> Vec<DelegationFinding> {
    let mut findings = Vec::new();
    let mut in_findings = false;
    for line in text.lines() {
        let cleaned = line.trim();
        if let Some(is_findings_section) = delegation_review_findings_section_heading(cleaned) {
            in_findings = is_findings_section;
            continue;
        }
        if !in_findings || line.chars().next().is_some_and(char::is_whitespace) {
            continue;
        }
        let finding = parse_delegation_review_actionable_finding_line(line)
            .or_else(|| parse_delegation_review_actionable_table_row(line))
            .or_else(|| {
                allow_compact_packet_lines
                    .then(|| parse_delegation_review_compact_finding_line(line))
                    .flatten()
            });
        if let Some(finding) = finding {
            findings.push(finding);
        }
    }
    dedupe_delegation_findings(findings)
}

fn parse_delegation_review_compact_finding_line(line: &str) -> Option<DelegationFinding> {
    let text = normalize_delegation_result_list_item(line);
    if text.is_empty() || text.starts_with('#') || text.starts_with('|') {
        return None;
    }
    parse_delegation_finding_line(line)
}

fn dedupe_delegation_findings(findings: Vec<DelegationFinding>) -> Vec<DelegationFinding> {
    let mut seen = std::collections::HashSet::new();
    findings
        .into_iter()
        .filter(|finding| {
            seen.insert((
                finding.severity.clone(),
                finding.file.clone(),
                finding.line,
                finding.message.clone(),
            ))
        })
        .take(MAX_DELEGATION_RESULT_FINDINGS)
        .collect()
}

fn delegation_review_findings_section_heading(cleaned: &str) -> Option<bool> {
    // Reviewer prose has several section styles. Only Actionable/Findings
    // sections should emit structured findings; known non-finding labels and
    // any new markdown heading reset capture so summaries/notes do not leak in.
    let label = delegation_review_section_label(cleaned)?;
    match label.as_str() {
        "actionable" | "findings" => Some(true),
        "changed files" | "changes reviewed" | "commands run" | "files inspected"
        | "informational" | "notes" | "reviewer summaries" | "summary" | "verification" => {
            Some(false)
        }
        _ if label.starts_with("findings:") => Some(true),
        // Severity headings refine the current Actionable/Informational
        // section; they must not independently switch capture on or off.
        _ if is_delegation_review_severity(&label) => None,
        _ if cleaned.starts_with('#') => Some(false),
        _ => None,
    }
}

fn delegation_review_section_label(cleaned: &str) -> Option<String> {
    let mut label = cleaned.trim();
    if label.starts_with('#') {
        label = label.trim_start_matches('#').trim();
    }
    if let Some(inner) = label
        .strip_prefix("**")
        .and_then(|value| value.strip_suffix("**"))
    {
        label = inner.trim();
    }
    label = label.strip_suffix(':').unwrap_or(label).trim();
    if label.is_empty() {
        return None;
    }
    Some(label.to_ascii_lowercase())
}

fn parse_delegation_review_actionable_finding_line(line: &str) -> Option<DelegationFinding> {
    let text = normalize_delegation_result_list_item(line);
    if text.is_empty() || is_delegation_no_findings_marker(text) {
        return None;
    }
    let (severity, rest) = parse_delegation_review_severity(text)?;
    if let Some((headline, details)) = split_delegation_review_bold_headline(rest) {
        let (file, line) = leading_delegation_review_location(details)
            .unwrap_or((None, None));
        return Some(DelegationFinding {
            severity: severity.to_owned(),
            file,
            line,
            message: format!("{headline} — {details}"),
        });
    }
    let (location, message) = split_delegation_review_location_and_message(rest)?;
    let (file, line) = parse_delegation_review_location(location);
    Some(DelegationFinding {
        severity: severity.to_owned(),
        file,
        line,
        message: message.to_owned(),
    })
}

fn parse_delegation_review_severity(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("**[") {
        let (severity, rest) = rest.split_once(']')?;
        if !is_delegation_review_severity(severity) {
            return None;
        }
        let rest = rest.trim();
        let rest = rest.strip_prefix("**").unwrap_or(rest).trim();
        return Some((severity.trim(), rest));
    }
    if let Some(rest) = text.strip_prefix("**") {
        let (severity, rest) = rest.split_once("**")?;
        if !is_delegation_review_severity(severity) {
            return None;
        }
        return Some((severity.trim(), rest.trim()));
    }
    if let Some(rest) = text.strip_prefix('[') {
        let (severity, rest) = rest.split_once(']')?;
        if !is_delegation_review_severity(severity) {
            return None;
        }
        return Some((severity.trim(), rest.trim()));
    }
    None
}

fn is_delegation_review_severity(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "critical" | "high" | "medium" | "low" | "note"
    )
}

fn split_delegation_review_bold_headline(text: &str) -> Option<(&str, &str)> {
    ["** — ", "** - ", "** – "]
        .iter()
        .find_map(|separator| text.split_once(separator))
        .map(|(headline, details)| (headline.trim(), details.trim()))
        .filter(|(headline, details)| !headline.is_empty() && !details.is_empty())
}

fn leading_delegation_review_location(text: &str) -> Option<(Option<String>, Option<u32>)> {
    let text = text.trim();
    if text.starts_with('[') {
        return Some(parse_delegation_review_location(text));
    }
    let rest = text.strip_prefix('`')?;
    let end = rest.find('`')?;
    Some(parse_delegation_review_location(&rest[..end]))
}

fn parse_delegation_review_actionable_table_row(line: &str) -> Option<DelegationFinding> {
    let line = line.trim();
    let row = line.strip_prefix('|')?.strip_suffix('|')?;
    let cells = row.split('|').map(str::trim).collect::<Vec<_>>();
    if cells.len() < 3 {
        return None;
    }
    let severity = cells[0]
        .trim_matches(|ch| matches!(ch, '*' | '`' | '[' | ']'))
        .trim();
    if !is_delegation_review_severity(severity) {
        return None;
    }
    let location = cells[1];
    let message = cells[2..].join(" | ");
    if location.is_empty() || message.trim().is_empty() {
        return None;
    }
    let (file, line) = parse_delegation_review_location(location);
    Some(DelegationFinding {
        severity: severity.to_owned(),
        file,
        line,
        message: message.trim().to_owned(),
    })
}

fn split_delegation_review_location_and_message(text: &str) -> Option<(&str, &str)> {
    [" — ", " - ", " – "]
        .iter()
        .find_map(|separator| text.split_once(separator))
        .map(|(location, message)| (location.trim(), message.trim()))
        .filter(|(location, message)| !location.is_empty() && !message.is_empty())
}

fn parse_delegation_review_location(location: &str) -> (Option<String>, Option<u32>) {
    let location = location.trim();
    if let Some((label, target)) = parse_markdown_link_location(location) {
        let parsed_label = parse_delegation_finding_location(label);
        let parsed_target = parse_delegation_finding_location(target);
        if parsed_label.0.is_some() && parsed_label.1.is_some() {
            return parsed_label;
        }
        if parsed_label.0.is_some() && parsed_target.1.is_some() {
            return (parsed_label.0, parsed_target.1);
        }
        if parsed_label.0.is_some() {
            return parsed_label;
        }
        return parsed_target;
    }
    parse_delegation_finding_location(location)
}

fn parse_markdown_link_location(location: &str) -> Option<(&str, &str)> {
    let location = location.trim();
    let label_end = location.strip_prefix('[')?.find("](")?;
    let label = &location[1..1 + label_end];
    let target_start = 1 + label_end + 2;
    let target_end = location[target_start..].find(')')? + target_start;
    Some((label, &location[target_start..target_end]))
}

fn parse_delegation_finding_head(head: &str) -> (&str, &str) {
    let head = head.trim();
    if let Some((severity, location)) = head.rsplit_once(char::is_whitespace) {
        if !severity.trim().is_empty() && looks_like_delegation_finding_location(location) {
            return (severity.trim(), location.trim());
        }
    }
    head.split_once(char::is_whitespace)
        .map(|(severity, location)| (severity.trim(), location.trim()))
        .unwrap_or((head, ""))
}

fn looks_like_delegation_finding_location(location: &str) -> bool {
    let location = location.trim().trim_matches('`');
    if location.is_empty() {
        return false;
    }
    if location.contains('/') || location.contains('\\') {
        return true;
    }
    location
        .rsplit_once(':')
        .is_some_and(|(file, line)| !file.trim().is_empty() && line.parse::<u32>().is_ok())
}

fn parse_delegation_finding_location(location: &str) -> (Option<String>, Option<u32>) {
    let location = location.trim().trim_matches('`');
    if location.is_empty() {
        return (None, None);
    }
    if let Some((file, line)) = location.rsplit_once(':') {
        let file = file.trim().trim_matches('`');
        let line = line.trim();
        let line_start = line.split_once('-').map(|(start, _)| start).unwrap_or(line);
        if let Ok(line) = line_start.parse::<u32>() {
            if !file.is_empty() {
                return (Some(file.to_owned()), Some(line));
            }
        }
        if !file.is_empty() {
            return (Some(file.to_owned()), None);
        }
    }
    (Some(location.to_owned()), None)
}

fn normalize_delegation_result_list_item(line: &str) -> &str {
    line.trim()
        .strip_prefix("- ")
        .or_else(|| line.trim().strip_prefix("* "))
        .unwrap_or_else(|| line.trim())
        .trim()
}

fn is_delegation_no_findings_marker(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "none" | "none." | "no findings" | "no findings." | "no issues found" | "no issues found."
    )
}

fn delegation_result_marker_start(text: &str) -> Option<usize> {
    let mut offset = 0;
    for segment in text.split_inclusive('\n') {
        let line = segment.trim_end_matches('\n').trim_end_matches('\r');
        if line.trim().eq_ignore_ascii_case("## Result") {
            return Some(offset);
        }
        offset += segment.len();
    }
    if offset < text.len() && text[offset..].trim().eq_ignore_ascii_case("## Result") {
        return Some(offset);
    }
    None
}

fn delegation_result_search_window(text: &str) -> &str {
    if text.len() <= DELEGATION_RESULT_PACKET_SEARCH_BYTES {
        return text;
    }
    let mut start = text.len() - DELEGATION_RESULT_PACKET_SEARCH_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}
