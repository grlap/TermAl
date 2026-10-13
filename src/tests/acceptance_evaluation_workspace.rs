// Tests for the workspace a verification record ran in, in acceptance
// evaluation briefs.
// Owns: the context-only marker on a record that ran in another workspace than
// the judged one at the same revision, on window records and on the criterion
// index's original-closure projection, and that a record of unknown workspace
// stays unmarked.
// Does not own: the off-revision marker or the binding text, which stay in
// acceptance_evaluation_bindings.rs, or criterion evidence selection, which
// stays in acceptance_evaluation_evidence.rs.
// A new module beside acceptance_evaluation_bindings.rs, declared from
// acceptance_evaluation.rs; it reuses that module's fixtures.
use super::*;

/// The source root the evaluation judges, as a named root's basis names it.
const JUDGED_WORKSPACE: &str = r"\\?\C:\repo\.tmp\wt-merger";
/// The producer's worktree, which held the same content when its check ran.
const PRODUCER_WORKSPACE: &str = r"\\?\C:\repo\.tmp\wt-producer";
/// The one revision both worktrees hold.
const REVISION: &str = "content-v1:4983be8a";

/// The record that closed criterion 2's obligation, in the producer's worktree.
const CLOSURE: &str = "cccccccccccccccccccccccccccccccc";
/// The same check, run again in the judged worktree.
const CURRENT: &str = "2222bbbb2222bbbb2222bbbb2222bbbb";
/// A record the tracker says ran in the producer's worktree.
const PEER: &str = "3333dddd3333dddd3333dddd3333dddd";
/// A record whose workspace the tracker does not say.
const UNKNOWN: &str = "4444eeee4444eeee4444eeee4444eeee";

fn window_record(locator: &str, workspace: Option<&str>) -> Value {
    let mut verification =
        json!({"check_kind": "test", "result": "passed", "source_revision": REVISION});
    if let Some(workspace) = workspace {
        verification["source_workspace"] = json!(workspace);
    }
    json!({
        "locator": locator, "kind": "verification", "family": "notes",
        "body_bytes": 22, "by": "greg/claude", "created_at": "2026-10-13T01:00:00Z",
        "non_holder": false, "summary": "fail-first test passed",
        "verification": verification
    })
}

/// A carried item judged in the merger's worktree, whose bound criterion 2
/// was closed by the producer's run at the same revision. With
/// `closure_in_window`, the window also holds the closing record whole, as
/// the tracker shows it today: without its workspace.
fn carried_task(closure_in_window: bool) -> AcceptanceEvaluationTask {
    let (mut show, full, mut page) = super::evidence_selection::canonical_binding_receipts();
    page["rows"][1]["binding"]["obligation"]["resolution"]["satisfaction"]["verification"]
        ["source_basis"] = json!({"workspace_id": PRODUCER_WORKSPACE, "source_revision": REVISION});
    let mut notes = vec![
        window_record(CURRENT, Some(JUDGED_WORKSPACE)),
        window_record(PEER, Some(PRODUCER_WORKSPACE)),
        window_record(UNKNOWN, None),
    ];
    if closure_in_window {
        notes.insert(0, window_record(CLOSURE, None));
    }
    show["notes"] = json!(notes);
    let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
    let rows = serde_json::from_value::<AcceptanceBindingEvidencePage>(page)
        .unwrap()
        .rows;
    apply_acceptance_binding_evidence(&mut task, rows).unwrap();
    task.judged_revision = Some(REVISION.to_owned());
    task.judged_workspace = Some(JUDGED_WORKSPACE.to_owned());
    task
}

fn workspace_marker() -> String {
    format!(
        "[context only: it ran in workspace {PRODUCER_WORKSPACE}, not in {JUDGED_WORKSPACE}, \
         the workspace this evaluation judges; not citable for a bound criterion]"
    )
}

/// The lines of `section` that list `locator` as a verification record.
fn record_lines<'a>(section: &'a str, locator: &str) -> Vec<&'a str> {
    section
        .lines()
        .filter(|line| line.contains(locator) && line.contains("(verification test"))
        .collect()
}

#[test]
fn a_closure_from_another_workspace_at_the_judged_revision_is_context_only() {
    let marker = workspace_marker();
    for closure_in_window in [false, true] {
        let task = carried_task(closure_in_window);
        let independent =
            build_acceptance_evaluator_brief(&task, "/repo", MAX_ACCEPTANCE_BRIEF_BYTES)
                .unwrap()
                .prompt;
        let same_session =
            build_same_session_acceptance_brief(&task, Some(REVISION), MAX_ACCEPTANCE_BRIEF_BYTES)
                .unwrap();
        // The criterion index offers the closure for criterion 2.
        for prompt in [&independent, &same_session] {
            assert!(
                prompt.contains(&format!("Criterion 2: {CLOSURE} (original obligation closure)")),
                "{closure_in_window}: the index offers the closure: {prompt}"
            );
        }
        // Where the independent brief lists the closure: in the index when only
        // the projection carries it, in the evidence list when the window holds
        // the record whole.
        let index_start = independent.find("Criterion evidence index").unwrap();
        let list_start = independent.find("Evidence recorded on the task").unwrap();
        let (index, list) = (
            &independent[index_start..list_start],
            &independent[list_start..],
        );
        let (listed, unlisted) = if closure_in_window { (list, index) } else { (index, list) };
        assert!(
            !record_lines(listed, CLOSURE).is_empty() && record_lines(unlisted, CLOSURE).is_empty(),
            "{closure_in_window}: the closure is listed once, where expected: {independent}"
        );
        for (name, prompt) in [
            ("independent", independent.as_str()),
            ("same_session", same_session.as_str()),
        ] {
            for locator in [CLOSURE, PEER] {
                let lines = record_lines(prompt, locator);
                assert!(!lines.is_empty(), "{closure_in_window} {name}: {locator} is listed: {prompt}");
                assert!(
                    lines.iter().all(|line| line.contains(&marker)),
                    "{closure_in_window} {name}: {locator} is context only: {lines:?}"
                );
            }
            for locator in [CURRENT, UNKNOWN] {
                let lines = record_lines(prompt, locator);
                assert!(!lines.is_empty(), "{closure_in_window} {name}: {locator} is listed: {prompt}");
                assert!(
                    lines.iter().all(|line| !line.contains("context only")),
                    "{closure_in_window} {name}: {locator} stays unmarked: {lines:?}"
                );
            }
        }
    }
}

#[test]
fn no_workspace_marker_without_a_judged_workspace() {
    // On a workdir the evaluation judges no workspace: a peer worktree's record
    // of the same revision is admitted, so nothing is marked for it.
    let mut task = carried_task(true);
    task.judged_workspace = None;
    let prompt = build_acceptance_evaluator_brief(&task, "/repo", MAX_ACCEPTANCE_BRIEF_BYTES)
        .unwrap()
        .prompt;
    assert!(!prompt.contains("context only: it ran in workspace"), "{prompt}");
    for locator in [CLOSURE, PEER, CURRENT, UNKNOWN] {
        assert!(!record_lines(&prompt, locator).is_empty(), "{locator}: {prompt}");
    }
}

#[test]
fn a_record_from_another_revision_keeps_the_revision_marker() {
    // Both differ: the revision is named, being the stronger reason.
    let mut task = carried_task(false);
    task.judged_revision = Some("content-v1:9c723064".to_owned());
    let prompt = build_acceptance_evaluator_brief(&task, "/repo", MAX_ACCEPTANCE_BRIEF_BYTES)
        .unwrap()
        .prompt;
    let lines = record_lines(&prompt, PEER);
    assert!(
        !lines.is_empty()
            && lines.iter().all(|line| line.contains(&format!(
            "[context only: it ran at {REVISION}, not at content-v1:9c723064"
        )) && !line.contains("ran in workspace")),
        "{lines:?}"
    );
}

#[test]
fn a_workspace_with_control_characters_is_unknown() {
    let mut show = show_receipt(None);
    show["notes"] = json!([window_record(PEER, Some("C:\\repo\nnext line"))]);
    let task = parse_acceptance_evaluation_task(show, full_receipt()).unwrap();
    assert_eq!(
        task.evidence[0]
            .verification
            .as_ref()
            .unwrap()
            .source_workspace,
        None
    );
}
