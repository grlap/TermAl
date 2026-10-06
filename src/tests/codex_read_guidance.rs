// The Windows read-only Codex reviewer's source-read guidance in the
// delegation prompt.
//
// Owns: tests that a Windows, Codex, read-only delegation's prompt carries the
// Unicode-safe native source-read guidance, that no other delegation's prompt
// does, and that the /review-code command carries the same text.
// Does not own: the rest of the delegation prompt, whose tests stay in
// delegations.rs, delegation_child_links.rs and kimi_read_only.rs.

use super::*;

/// The guidance's opening words.
const CODEX_READ_GUIDANCE_START: &str = "Windows read-only Codex inspection:";

/// The /review-code command, which carries the same guidance for reviewers.
const REVIEW_CODE_COMMAND: &str = include_str!("../../.claude/commands/review-code.md");

fn delegation_record(agent: Agent, write_policy: DelegationWritePolicy) -> DelegationRecord {
    DelegationRecord {
        id: "delegation-read-guidance".to_owned(),
        parent_session_id: "session-parent".to_owned(),
        child_session_id: "session-child".to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Reviewer".to_owned(),
        prompt: "/review-code".to_owned(),
        cwd: "/tmp".to_owned(),
        agent,
        model: None,
        write_policy,
        created_at: stamp_now(),
        started_at: None,
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 0,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    }
}

fn read_only_codex() -> DelegationRecord {
    delegation_record(Agent::Codex, DelegationWritePolicy::ReadOnly)
}

/// On Windows a read-only Codex delegation is told to read source through
/// native Git or rg output, never through PowerShell's pipeline rendering.
#[test]
fn a_windows_read_only_codex_delegation_gets_the_native_source_read_guidance() {
    let prompt = build_delegation_prompt_on(&read_only_codex(), true);

    assert!(prompt.contains(CODEX_READ_GUIDANCE_START), "{prompt}");
    assert!(
        prompt.ends_with(WINDOWS_CODEX_READ_ONLY_SOURCE_READS),
        "{prompt}"
    );
    assert_eq!(
        prompt.matches(CODEX_READ_GUIDANCE_START).count(),
        1,
        "{prompt}"
    );
}

/// The real prompt builder passes the host's own platform.
#[test]
fn the_delegation_prompt_follows_the_host_platform() {
    let record = read_only_codex();

    assert_eq!(
        build_delegation_prompt(&record),
        build_delegation_prompt_on(&record, cfg!(windows))
    );
}

/// No other delegation gets the guidance: not off Windows, not a writable
/// Codex delegation, and not a Claude or Kimi one.
#[test]
fn no_other_delegation_gets_the_native_source_read_guidance() {
    let cases = [
        ("read-only Codex off Windows", read_only_codex(), false),
        (
            "Codex in a shared worktree on Windows",
            delegation_record(
                Agent::Codex,
                DelegationWritePolicy::SharedWorktree {
                    owned_paths: vec!["src".to_owned()],
                },
            ),
            true,
        ),
        (
            "Codex in an isolated worktree on Windows",
            delegation_record(
                Agent::Codex,
                DelegationWritePolicy::IsolatedWorktree {
                    owned_paths: vec!["src".to_owned()],
                    worktree_path: None,
                },
            ),
            true,
        ),
        (
            "read-only Claude on Windows",
            delegation_record(Agent::Claude, DelegationWritePolicy::ReadOnly),
            true,
        ),
        (
            "read-only Kimi on Windows",
            delegation_record(Agent::Kimi, DelegationWritePolicy::ReadOnly),
            true,
        ),
    ];
    for (case, record, windows) in cases {
        let prompt = build_delegation_prompt_on(&record, windows);

        assert!(
            !prompt.contains(CODEX_READ_GUIDANCE_START),
            "{case}: {prompt}"
        );
    }
}

/// The Claude and Kimi prompts, and the vendor-neutral write-policy text, do
/// not depend on the platform.
#[test]
fn claude_and_kimi_prompts_are_the_same_on_every_platform() {
    for agent in [Agent::Claude, Agent::Kimi] {
        let record = delegation_record(agent, DelegationWritePolicy::ReadOnly);

        assert_eq!(
            build_delegation_prompt_on(&record, true),
            build_delegation_prompt_on(&record, false),
            "{agent:?}"
        );
    }
    let off_windows = build_delegation_prompt_on(&read_only_codex(), false);
    let on_windows = build_delegation_prompt_on(&read_only_codex(), true);
    assert_eq!(
        on_windows,
        format!("{off_windows}\n\n{WINDOWS_CODEX_READ_ONLY_SOURCE_READS}"),
        "only the guidance is added for Codex on Windows"
    );
}

/// The guidance does not claim to enforce fidelity: it tells the reviewer to
/// report inspection unavailable when the exact text or full coverage cannot
/// be had.
#[test]
fn the_guidance_asks_for_inspection_unavailable_rather_than_claiming_fidelity() {
    assert!(WINDOWS_CODEX_READ_ONLY_SOURCE_READS.ends_with(
        "If exact text or complete coverage cannot be obtained, report inspection unavailable rather than a clean review."
    ));
    assert!(WINDOWS_CODEX_READ_ONLY_SOURCE_READS
        .contains("nonterminating errors can still leave exit code 0"));
}

/// The /review-code command carries the same text, word for word.
#[test]
fn the_review_code_command_carries_the_same_guidance() {
    let normalized = REVIEW_CODE_COMMAND.replace("\r\n", "\n");

    assert!(
        normalized
            .lines()
            .any(|line| line == WINDOWS_CODEX_READ_ONLY_SOURCE_READS),
        "the command's guidance line differs from the prompt's"
    );
}
