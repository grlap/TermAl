// Windows Codex source-read and writable-shell guidance in delegation prompts.
//
// Owns: tests that a Windows, Codex, read-only delegation's prompt carries the
// Unicode-safe native source-read guidance, that no other delegation's prompt
// does, and that the /review-code command carries the same text. Also covers
// parser and encoding guidance for every writable Codex delegation mode.
// Does not own: the rest of the delegation prompt, whose tests stay in
// delegations.rs, delegation_child_links.rs and kimi_read_only.rs.

use super::*;

/// The guidance's opening words.
const CODEX_READ_GUIDANCE_START: &str = "Windows read-only Codex inspection:";
const CODEX_WRITE_GUIDANCE_START: &str = "Windows Codex writable-child shell:";

/// The /review-code command, which carries the same guidance for reviewers.
const REVIEW_CODE_COMMAND: &str = include_str!("../../.claude/commands/review-code.md");

fn delegation_record(agent: Agent, write_policy: DelegationWritePolicy) -> DelegationRecord {
    DelegationRecord {
        original_project_id: None,
        project_basis_recorded: false,
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
        base_revision: None,
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

#[test]
fn windows_writable_codex_briefs_explain_parser_errors_and_encoding_without_translation() {
    for mode in [
        DelegationMode::Worker,
        DelegationMode::Reviewer,
        DelegationMode::Explorer,
    ] {
        for write_policy in [
            DelegationWritePolicy::IsolatedWorktree {
                owned_paths: vec!["src".to_owned()],
                worktree_path: None,
                base: None,
            },
            DelegationWritePolicy::SharedWorktree {
                owned_paths: vec!["src".to_owned()],
            },
        ] {
            let mut child = delegation_record(Agent::Codex, write_policy);
            child.mode = mode;
            child.prompt = "Run the requested check".to_owned();
            let prompt = build_delegation_prompt_on(&child, true);
            for detail in [
                "Windows PowerShell 5.1",
                "rejects && and ||",
                "parser error",
                "do not silently translate",
                "Do not write or rewrite source through PowerShell text cmdlets",
                "Get-Content/Set-Content pipelines and Out-File",
                "> / >> redirection",
                "corrupt non-ASCII bytes or change BOMs while exiting 0",
                "Use apply_patch or native editing tools that preserve the file's encoding",
                "inspect git diff for unintended encoding changes",
                "workspace-write sandbox or approval policy",
            ] {
                assert!(prompt.contains(detail), "missing {detail}: {prompt}");
            }
            assert_eq!(prompt.matches(CODEX_WRITE_GUIDANCE_START).count(), 1);
            assert!(prompt.contains(&child.prompt));
            assert!(!prompt.contains(CODEX_READ_GUIDANCE_START));
            assert!(!build_delegation_prompt_on(&child, false)
                .contains(CODEX_WRITE_GUIDANCE_START));
            child.agent = Agent::Claude;
            assert!(!build_delegation_prompt_on(&child, true)
                .contains(CODEX_WRITE_GUIDANCE_START));
        }
    }
    assert!(!build_delegation_prompt_on(&read_only_codex(), true)
        .contains(CODEX_WRITE_GUIDANCE_START));
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
                    base: None,
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
