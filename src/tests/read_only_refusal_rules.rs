// Owns the tests that a read-only Claude child's refused request is told
// which rule it broke and what form is allowed, that the refusal never calls
// an acceptance evaluator a reviewer, and that the read-only command forms an
// evaluator needs to check a freeze fingerprint stay allowed. Does not own
// which commands the gate allows in general (claude.rs and
// claude_permission_boundary.rs tests), the evaluator's tracker access
// (evaluator_tool_access.rs) or the evaluator brief (acceptance_evaluation.rs).
// New file.
use super::delegation_support::test_app_state_with_delegation_codex_runtime;
use super::evaluator_tool_access::{delegation_child, tracker_project};
use super::*;

const WORKSPACE: &str = "/work/repo";

/// The host's answer to one request of a read-only child: `Ok` when it is
/// allowed, the refusal's text otherwise.
fn read_only_answer(
    tool: &str,
    input: Value,
    approval_mode: ClaudeApprovalMode,
    delegation_child: bool,
    cwd: &str,
) -> std::result::Result<(), String> {
    let action = classify_claude_control_request(
        &json!({"type":"control_request", "request_id":"refusal-rules",
            "request":{"subtype":"can_use_tool", "tool_name":tool, "input":input}}),
        &mut ClaudeTurnState::default(),
        approval_mode,
        delegation_child,
        cwd,
        false,
    )
    .unwrap()
    .expect("the request reaches the host's gate");
    match action {
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. }) => Ok(()),
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny { message, .. }) => {
            Err(message)
        }
        _ => panic!("a read-only child's request is answered at once"),
    }
}

fn bash_refusal(command: &str) -> String {
    read_only_answer(
        "Bash",
        json!({ "command": command }),
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        true,
        WORKSPACE,
    )
    .expect_err(command)
}

/// Every refusal names the gate, never a reviewer, and says what is allowed.
fn assert_refusal_shape(refusal: &str, context: &str) {
    assert!(
        refusal.contains("this Claude delegation is read-only"),
        "{context}: {refusal}"
    );
    assert!(!refusal.contains("reviewer"), "{context}: {refusal}");
    assert!(refusal.contains(" Rule: "), "{context}: {refusal}");
    assert!(refusal.contains(" Allowed: "), "{context}: {refusal}");
}

#[test]
fn a_refused_bash_request_names_the_rule_it_broke_and_the_allowed_form() {
    for (command, rule) in [
        // Chaining with `;` or a line break.
        ("git status; git log -1", "`;` and line breaks"),
        ("git status\ngit log -1", "`;` and line breaks"),
        // Redirection.
        ("git diff HEAD > change.patch", "redirection"),
        ("cat < notes.txt", "redirection"),
        // Substitution.
        ("echo $(whoami)", "command substitution"),
        ("echo `whoami`", "command substitution"),
        // A background job.
        ("git status & git log -1", "background `&`"),
        // Git pointed at another repository or configuration.
        ("git -C /work/other diff --stat", "git's `-C` option"),
        (
            "git --git-dir=/work/other/.git status",
            "git's `--git-dir` option",
        ),
        (
            "git --work-tree=/work/other status",
            "git's `--work-tree` option",
        ),
        ("git -c core.pager=less log -1", "git's `-c` option"),
        // A `cd` away from the workspace before git.
        (
            "cd /work/other && git status",
            "`cd` away from the workspace",
        ),
        // A command or git subcommand outside the read-only list.
        ("cargo test", "`cargo` is not one of the read-only commands"),
        (
            "git push",
            "`git push` is not one of the read-only commands",
        ),
    ] {
        let refusal = bash_refusal(command);
        assert_refusal_shape(&refusal, command);
        assert!(refusal.contains(rule), "{command}: {refusal}");
        // The allowed form is the same for every Bash refusal: one command per
        // call in the workspace, or read-only commands joined with && or |.
        assert!(
            refusal.contains("one read-only command per call"),
            "{command}: {refusal}"
        );
        assert!(refusal.contains("`&&` or `|`"), "{command}: {refusal}");
        assert!(refusal.contains(WORKSPACE), "{command}: {refusal}");
    }
}

// `cmp` compares two files and writes only to stdout, so a read-only child can
// check that two files are identical without hashing them. Redirection and
// chaining stay refused around it, and `git hash-object` stays refused: it runs
// gitattributes clean filters.
#[test]
fn a_read_only_delegation_may_compare_two_files_with_cmp() {
    for command in [
        "cmp a b",
        "cmp -s a b",
        "cmp a b | cat",
        "cmp AGENTS.md CLAUDE.md",
    ] {
        read_only_answer(
            "Bash",
            json!({ "command": command }),
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            true,
            WORKSPACE,
        )
        .unwrap_or_else(|refusal| panic!("{command} should be allowed: {refusal}"));
    }
    for (command, rule) in [
        ("cmp a b > f", "redirection"),
        ("cmp a b >> f", "redirection"),
        ("cmp a b; touch f", "`;` and line breaks"),
        ("git hash-object a", "`git hash-object` is not one"),
        ("git hash-object -w a", "`git hash-object` is not one"),
        (
            "git diff | git hash-object --stdin",
            "`git hash-object` is not one",
        ),
    ] {
        let refusal = bash_refusal(command);
        assert_refusal_shape(&refusal, command);
        assert!(refusal.contains(rule), "{command}: {refusal}");
        assert!(
            !refusal.contains("`cmp` is not one"),
            "{command}: {refusal}"
        );
    }
}

// The rule named is the one the check applied first, on lines that break more
// than one rule or spell a command in a way the shell de-quotes.
#[test]
fn a_refusal_names_the_rule_the_check_applied_first_on_a_mixed_line() {
    for (command, rule) in [
        // The check vets every `cd` of a git line before any command.
        (
            "git -C /work/other status && cd /work/else",
            "`cd` away from the workspace",
        ),
        (
            "cargo test && cd /work/else && git status",
            "`cd` away from the workspace",
        ),
        ("ls * && cd /work/else && git status", "`cd` away"),
        ("cd && git status", "a bare `cd`"),
        // Quoted or escaped heads are read as the shell runs them.
        ("'git' -C /work/other status", "git's `-C` option"),
        ("g\\it -C /work/other status", "git's `-C` option"),
        (
            "'cd' /work/other && git status",
            "`cd` away from the workspace",
        ),
        // A read-only command does not take the blame for its neighbour.
        ("true && cargo test", "`cargo` is not one"),
        ("git status && cargo test", "`cargo` is not one"),
        ("git status 2>/dev/null && cargo test", "`cargo` is not one"),
        // An `&` inside a redirection is named as the redirection it is.
        ("git status 2>&1 | head", "including `2>&1`"),
        ("git log &> log.txt", "including `2>&1`"),
        // Characters refused even inside quotes say so.
        ("grep 'a;b' notes.txt", "even inside quotes"),
        ("git log --format='%h > %s'", "even inside quotes"),
        // Git help, git without a subcommand, and the listing-only subcommands.
        ("git --help", "git help is refused"),
        ("git log --help", "git help is refused"),
        ("git", "without a subcommand"),
        (
            "git remote add origin https://example.com/x.git",
            "only as a listing",
        ),
        ("git branch new-branch", "only as a listing"),
        (
            "git diff --output=change.patch",
            "`git diff` is allowed, but",
        ),
        // Shell syntax the check cannot read, and an empty command.
        ("ls *", "this shell syntax is refused"),
        ("git status &&  && git log", "an empty command"),
        // A loop whose body breaks a rule says how to find which.
        (
            "for d in a b; do git -C $d status; done",
            "run a body command alone",
        ),
    ] {
        let refusal = bash_refusal(command);
        assert_refusal_shape(&refusal, command);
        assert!(refusal.contains(rule), "{command}: {refusal}");
    }
}

// The refusal's command lists mirror the checker's dispatch. Each listed git
// subcommand and option-checked command has a form the checker allows, so a
// refusal that calls it "allowed, but" is true, and git subcommands outside
// the list are refused by the checker and named as such.
#[test]
fn the_refusals_command_lists_match_what_the_checker_allows() {
    let git_forms = [
        ("diff", "git diff --stat"),
        ("log", "git log -1 --oneline"),
        ("show", "git show --stat HEAD"),
        ("blame", "git blame src/main.rs"),
        ("describe", "git describe --tags"),
        ("ls-files", "git ls-files"),
        ("rev-parse", "git rev-parse HEAD"),
        ("shortlog", "git shortlog -s"),
        ("status", "git status --short"),
        ("patch-id", "git patch-id"),
        ("grep", "git grep -n refusal"),
        ("remote", "git remote -v"),
        ("branch", "git branch --show-current"),
    ];
    assert_eq!(
        git_forms.iter().map(|(sub, _)| *sub).collect::<Vec<_>>(),
        CLAUDE_READ_ONLY_GIT_SUBCOMMANDS
    );
    for (_, command) in git_forms {
        assert!(
            claude_bash_command_is_read_only(command, WORKSPACE),
            "{command}"
        );
    }
    let option_checked = [
        ("date", "date +%Y"),
        ("rg", "rg -n refusal src"),
        ("find", "find src -name x.rs"),
        ("sed", "sed -n 1p src/main.rs"),
        ("git", "git status"),
    ];
    assert_eq!(
        option_checked
            .iter()
            .map(|(head, _)| *head)
            .collect::<Vec<_>>(),
        CLAUDE_OPTION_CHECKED_BASH_COMMANDS
    );
    for (_, command) in option_checked {
        assert!(
            claude_bash_command_is_read_only(command, WORKSPACE),
            "{command}"
        );
    }
    for subcommand in [
        "add",
        "am",
        "apply",
        "bisect",
        "cat-file",
        "checkout",
        "cherry-pick",
        "clean",
        "clone",
        "commit",
        "config",
        "fetch",
        "gc",
        "init",
        "merge",
        "mv",
        "notes",
        "pull",
        "push",
        "rebase",
        "reflog",
        "reset",
        "restore",
        "revert",
        "rm",
        "stash",
        "submodule",
        "switch",
        "tag",
        "worktree",
    ] {
        let command = format!("git {subcommand}");
        assert!(
            !claude_bash_command_is_read_only(&command, WORKSPACE),
            "{command}"
        );
        let refusal = bash_refusal(&command);
        assert!(
            refusal.contains(&format!("`git {subcommand}` is not one")),
            "{command}: {refusal}"
        );
    }
}

// Text from the request comes back on one bounded line, with no control,
// separator or invisible format character in it.
#[test]
fn a_refusal_quotes_request_text_on_one_bounded_line() {
    let tool = format!(
        "Evil\u{7}\u{202E}\u{2028}\u{200B}\u{FEFF}Tool{}",
        "x".repeat(500)
    );
    let refusal = read_only_answer(
        &tool,
        json!({}),
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        true,
        WORKSPACE,
    )
    .expect_err("an unknown tool is refused");
    assert_refusal_shape(&refusal, "unknown tool");
    for hidden in [
        '\u{7}', '\u{202E}', '\u{2028}', '\u{200B}', '\u{FEFF}', '\n',
    ] {
        assert!(!refusal.contains(hidden), "{hidden:?}: {refusal}");
    }
    assert!(refusal.contains("x…`"), "{refusal}");
    assert!(refusal.chars().count() < 600, "{refusal}");
    let command = format!("cargo{} test", "\u{202E}".repeat(3));
    let refusal = bash_refusal(&command);
    assert!(!refusal.contains('\u{202E}'), "{refusal}");
}

#[test]
fn a_refused_tool_request_names_the_rule_for_that_tool() {
    for (tool, input, rule) in [
        (
            "PowerShell",
            json!({"command":"git status"}),
            "the PowerShell tool is always refused",
        ),
        (
            "Write",
            json!({"file_path":"x", "content":"y"}),
            "may not write or edit files",
        ),
        (
            "Edit",
            json!({"file_path":"x", "old_string":"a", "new_string":"b"}),
            "may not write or edit files",
        ),
        (
            "Agent",
            json!({"prompt":"look"}),
            "`Agent` is not one of the tools",
        ),
        (
            "mcp__engram__show",
            json!({"work_ref":"w-task"}),
            "has no tracker access",
        ),
    ] {
        let refusal = read_only_answer(
            tool,
            input,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            true,
            WORKSPACE,
        )
        .expect_err(tool);
        assert_refusal_shape(&refusal, tool);
        assert!(refusal.contains(rule), "{tool}: {refusal}");
    }
}

// The gate is shared by reviewers and acceptance evaluators, so its refusal
// must not call an evaluator a reviewer, and the command forms an evaluator
// needs to check a freeze fingerprint must pass it.
#[test]
fn an_evaluator_child_is_refused_as_a_delegation_and_may_check_a_fingerprint() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("evaluator-refusal-rules");
    let (project_id, root) = tracker_project(&state, "evaluator-refusal-rules-project");
    let parent = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let evaluator = delegation_child(
        &state,
        &parent,
        &project_id,
        &root,
        Agent::Claude,
        DelegationMode::Evaluator,
    );
    let (approval_mode, is_child) = state
        .claude_control_request_context(&evaluator)
        .expect("the evaluator's control context should resolve");
    assert_eq!(approval_mode, ClaudeApprovalMode::ReadOnlyAutoApprove);
    let cwd = root.to_string_lossy().into_owned();

    for (tool, input) in [
        (
            "Bash",
            json!({"command":"git -C /work/other diff --cached --stat"}),
        ),
        (
            "Bash",
            json!({"command":"git diff --cached --stat; git status"}),
        ),
        ("PowerShell", json!({"command":"git diff --cached --stat"})),
        ("mcp__engram__show", json!({"work_ref":"w-task"})),
    ] {
        let refusal =
            read_only_answer(tool, input.clone(), approval_mode, is_child, &cwd).expect_err(tool);
        assert_refusal_shape(&refusal, &format!("{tool} {input}"));
    }

    for command in [
        "git diff --cached --stat",
        "git diff HEAD --stat",
        "git diff HEAD -- . | sha256sum",
        "git status --short && git log -1 --oneline",
    ] {
        assert_eq!(
            read_only_answer(
                "Bash",
                json!({ "command": command }),
                approval_mode,
                is_child,
                &cwd
            ),
            Ok(()),
            "{command}"
        );
    }
}
