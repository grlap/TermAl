// Claude Code CLI recorder and turn-state tests.
//
// The anthropic/claude-code CLI emits an NDJSON stream on stdout that TermAl
// parses via `handle_claude_stdout_message` in `src/runtime.rs`. Each line is
// an `assistant`, `user`, `stream_event`, or `result` envelope.
// `ClaudeTurnState` accumulates per-turn bookkeeping — pending tool uses
// keyed by `tool_use_id`, parallel sub-agents spawned via the `task` tool,
// the streamed assistant text buffer, and approval keys already seen — and
// is finalized by a `result` event or torn down when the runtime exits.
//
// Streamed text reconciliation is the trickiest seam: Claude emits a stream
// of `text_delta` chunks and then a final full-text payload inside an
// `assistant` frame after `message_stop`. `handle_claude_streamed_text` must
// append the missing suffix when the final is longer, skip the duplicate
// when the final matches, and REPLACE the bubble when the final diverges.
// Parallel agents (the `task` tool) spawn sub-recorders that fan progress
// into the parent transcript; their tool-use / tool-result / tool-error
// frames are folded into `ParallelAgentProgress` entries and recorded as
// subagent results. Transcript boundary: a `tool_use` arriving after
// streamed text ends must start a follow-up `Message`, not append to the
// closed text bubble. Production surfaces under test live in
// `src/runtime.rs`: `handle_claude_stdout_message`, `handle_claude_tool_use`,
// `handle_claude_tool_result`, the `handle_claude_task_tool_*` family,
// `handle_claude_streamed_text`, and `handle_claude_result`.

use super::*;

fn claude_permission_request(tool_name: &str, tool_input: Value) -> Value {
    json!({
        "type": "control_request",
        "request_id": "permission-request-1",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": tool_name,
            "input": tool_input
        }
    })
}

// Pins read-only auto-approval as a filtered Claude permission mode, not a
// shortcut to full `AutoApprove`. Read-only Bash commands may proceed without
// surfacing an approval card so `/review-local` can finish unattended.
#[test]
fn claude_read_only_auto_approve_allows_read_only_bash_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Bash",
            json!({
                "command": "git diff --cached -- src/delegations.rs | head -40"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        "C:/reviewer-sandbox",
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
        request_id,
        updated_input,
    }) = action
    else {
        panic!("read-only bash permission should be auto-allowed");
    };

    assert_eq!(request_id, "permission-request-1");
    assert_eq!(
        updated_input.get("command").and_then(Value::as_str),
        Some("git diff --cached -- src/delegations.rs | head -40")
    );
}

#[test]
fn claude_read_only_auto_approve_allows_review_local_bash_shapes() {
    for command in [
        "git status",
        "git status --short",
        "git diff --cached -- src/delegations.rs",
        "git diff --name-only && git diff --cached --name-only",
        "git ls-files --others --exclude-standard",
        "git --no-pager log",
        "git --no-pager diff --cached",
        "git diff --stat && echo \"=== X ===\" && git diff --name-only",
        "git remote -v",
        "git describe --tags --always",
        "git blame -L 10,20 src/claude.rs",
        "git --no-pager shortlog -sn",
        "git -P log",
        "git --no-optional-locks status",
        "git branch",
        "git branch -a",
        "git branch -vv",
        "git branch --list",
        "git branch --sort=-committerdate",
        "git log --grep='fix(scope)' -n 5",
        "git diff --text",
        "git shortlog -sn HEAD",
        "git grep -n TODO",
        "git grep -e ReadOnly",
        "git grep -eReadOnly",
        "git grep --text pattern",
        "cd ui && cat package.json",
        "find .claude/reviewers -name \"*.md\" 2>/dev/null",
        "grep -n ReadOnlyAutoApprove src/claude.rs | head -20",
        "grep -n 'two words' docs/bugs.md",
        "sed -n 1,120p src/claude.rs",
        "sed -e 's/window/door/' src/main.rs",
        "sed -e 's/^/word /' src/main.rs",
        "grep -n 'a & b' docs/bugs.md",
        "cat docs/bugs.md | tail -40",
        "wc -l src/claude.rs",
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_permission_request("Bash", json!({ "command": command })),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            "C:/reviewer-sandbox",
        )
        .unwrap()
        .expect("permission request should be classified");

        let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        }) = action
        else {
            panic!("read-only review-local command should be auto-allowed: {command}");
        };

        assert_eq!(request_id, "permission-request-1");
        assert_eq!(
            updated_input.get("command").and_then(Value::as_str),
            Some(command)
        );
    }
}

// Exercises `claude_bash_command_is_read_only` (through the full permission classifier)
// for read-only git *content* commands reviewers depend on. tm-9c5 fixed two over-broad
// denials while keeping the write boundary intact:
//   * `cd <cwd> && git …` was rejected wholesale by the cd+git exec-sink guard, even
//     though a `cd` into the reviewer's OWN working directory is a no-op — byte-for-byte
//     identical to running the git command with no `cd`. Reviewers `cd` into the target
//     repo first, so this silently killed their entire git surface -> INCONCLUSIVE.
//   * hashers (`sha256sum`, `md5sum`, `cksum`, `git patch-id`) were absent from the
//     read-only allow-lists, so diff-fingerprinting was blocked.
// The security boundary is exact-cwd-only: a `cd` into a *different* repo, or into a
// subdir (which may carry a nested `.git`), still fails closed because that genuinely
// retargets git the way `-C` / `--git-dir` do — AND that detection is tokenized, so a
// quoted/escaped `'git'` / `"git"` / `g\it` cannot slip past the guard (tm-cnq). `git
// hash-object` is deliberately excluded: it runs gitattributes clean filters, an exec sink.
fn claude_bash_is_read_only_for_test(command: &str, cwd: &str) -> bool {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request("Bash", json!({ "command": command })),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        cwd,
    )
    .unwrap()
    .expect("permission request should be classified");
    matches!(
        action,
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. })
    )
}

#[test]
fn read_only_git_content_checker_allows_reviewer_git_and_hashers() {
    // The reviewer child's own working directory, pre-normalized exactly as the runtime
    // does before handing it to the classifier.
    let cwd = normalize_local_user_facing_path("C:/github/Personal/TermAl");
    let allowed = [
        "git diff",
        "git --no-pager diff",
        "git --no-pager diff --binary --no-ext-diff --no-textconv --no-color",
        "git --no-pager diff --binary --no-ext-diff --no-textconv --no-color | wc -c",
        "git --no-pager diff --binary --no-ext-diff --no-textconv --no-color | cat",
        "git diff | wc -c",
        "git diff | cat",
        // Hashers as pipe targets — diff fingerprinting (tm-9c5).
        "git --no-pager diff --binary --no-ext-diff --no-textconv --no-color | sha256sum",
        "git diff | md5sum",
        "git diff | cksum",
        "git --no-pager diff --no-ext-diff --no-textconv --no-color | git patch-id --stable",
        // `cd` into the reviewer's OWN cwd is a no-op; git content stays read-only.
        "cd \"C:/github/Personal/TermAl\" && git rev-parse HEAD",
        "cd \"C:/github/Personal/TermAl\" && git --no-pager diff --no-ext-diff --no-textconv --no-color | wc -c",
        "cd \"C:/github/Personal/TermAl\" && git diff | cat",
        "cd \"C:/github/Personal/TermAl\" && git diff | sha256sum",
        // Tokenized detection routes even a quoted `git` through the cd-guard; into the
        // OWN cwd it still passes, so the tm-cnq fix must not over-deny same-cwd quoting.
        "cd \"C:/github/Personal/TermAl\" && 'git' status",
        // The `cd` HEAD is tokenized too, so a quoted `cd` into the OWN cwd is the same
        // no-op as an unquoted one and stays allowed (tm-b9k).
        "'cd' \"C:/github/Personal/TermAl\" && git status",
        // `cd .` is always a no-op regardless of cwd.
        "cd . && git diff",
    ];
    for command in allowed {
        assert!(
            claude_bash_is_read_only_for_test(command, &cwd),
            "expected allowed (read-only): {command}"
        );
    }

    let denied = [
        // A subdir may carry a nested `.git`, so cd into it still retargets git.
        "cd \"C:/github/Personal/TermAl/ui\" && git diff",
        // A different repo entirely.
        "cd \"C:/other/repo\" && git diff",
        // Bare `cd` (-> $HOME) and `cd ~` are not the cwd.
        "cd && git diff",
        "cd ~ && git diff",
        // A quoted/escaped `git` after `cd <other repo>` must NOT slip past the cd-guard:
        // the tokenizer de-quotes it to a real git command, so detection must too (tm-cnq).
        "cd \"C:/other/repo\" && 'git' status",
        "cd \"C:/other/repo\" && \"git\" status",
        "cd \"C:/other/repo\" && g\\it status",
        // The mirror image (tm-b9k): a quoted/escaped `cd` retargeting git must ALSO be
        // caught. The cd-guard decides "is this a cd" from tokens for exactly the tm-cnq
        // reason — the tokenizer de-quotes `'cd'` into a real cd — and the approval pass
        // accepts a tokenized `cd <target>`, so raw-text detection here would skip the
        // cwd check and hand back a git retargeted at another repo.
        "'cd' \"C:/other/repo\" && git status",
        "\"cd\" \"C:/other/repo\" && git status",
        "c\\d \"C:/other/repo\" && git status",
        // `git hash-object` is fully denied (clean-filter exec sink), with or without `-w`.
        "git hash-object src/claude.rs",
        "git hash-object -w src/claude.rs",
        "git diff | git hash-object --stdin",
        // Genuine writes stay denied, unaffected by the cwd allowance.
        "git commit -m x",
        "echo mutated > README.md",
    ];
    for command in denied {
        assert!(
            !claude_bash_is_read_only_for_test(command, &cwd),
            "expected denied: {command}"
        );
    }
}

// The runtime cwd is stored with backslashes on Windows while the agent writes `cd`
// with forward slashes, so the same-folder allowance must compare paths
// separator-insensitively (and case-insensitively on Windows). A raw string `==`
// rejected the exact reviewer command `cd "C:/github/Personal/PhoenixCodeNav" && git ...`.
#[test]
fn read_only_checker_same_folder_cd_matches_across_separator() {
    let backslash_cwd = "C:\\github\\Personal\\PhoenixCodeNav";
    // Forward-slash `cd` into a backslash cwd — the exact denied shape from the field.
    assert!(
        claude_bash_is_read_only_for_test(
            "cd \"C:/github/Personal/PhoenixCodeNav\" && git rev-parse --show-toplevel",
            backslash_cwd,
        ),
        "forward-slash cd into a backslash cwd must be allowed"
    );
    assert!(
        claude_bash_is_read_only_for_test(
            "cd \"C:/github/Personal/PhoenixCodeNav\" && git --no-optional-locks status --short",
            backslash_cwd,
        ),
        "forward-slash cd + git status into a backslash cwd must be allowed"
    );
    // Reverse: backslash `cd` into a forward-slash cwd.
    assert!(
        claude_bash_is_read_only_for_test(
            "cd \"C:\\github\\Personal\\PhoenixCodeNav\" && git diff",
            "C:/github/Personal/PhoenixCodeNav",
        ),
        "backslash cd into a forward-slash cwd must be allowed"
    );
    // A different repo, and a subdirectory (may hold a nested .git), stay denied
    // regardless of separators.
    assert!(
        !claude_bash_is_read_only_for_test(
            "cd \"C:/github/Personal/OtherRepo\" && git diff",
            backslash_cwd,
        ),
        "a different repo must stay denied"
    );
    assert!(
        !claude_bash_is_read_only_for_test(
            "cd \"C:/github/Personal/PhoenixCodeNav/src\" && git diff",
            backslash_cwd,
        ),
        "a subdirectory must stay denied"
    );
}

// Windows filesystems are case-insensitive, so a case-only difference is the same dir.
#[test]
#[cfg(windows)]
fn read_only_checker_same_folder_cd_is_case_insensitive_on_windows() {
    assert!(
        claude_bash_is_read_only_for_test(
            "cd \"c:/GitHub/personal/PHOENIXCODENAV\" && git diff",
            "C:\\github\\Personal\\PhoenixCodeNav",
        ),
        "case-only difference must be allowed on Windows"
    );
}

// The Windows PowerShell tool is denied wholesale for read-only reviewers.
//
// It used to route through the Bash reader, which implements BASH grammar. Every
// PowerShell-specific construct that reader mis-modelled became a security defect:
// `(...)`/`@(...)` sub-expression evaluation (tm-mdx, survived only because the
// tokenizer happens to fail closed on `(`), the `2>/dev/null` strip writing
// `<drive>\dev\null` (tm-1ex), the `cd ` head reaching `continue` before the
// tokenizer and giving an arbitrary-path WRITE (tm-hfh), and `\` de-escaped per
// bash so `g\it` read as `git` while PowerShell executes `.\g\it` FROM THE
// REVIEWED TREE — arbitrary code execution (tm-jk7).
//
// So this pins the structural rule, not another denylist: a bash parser gates only
// bash. Each historical escape is listed as a case so that re-introducing a
// PowerShell arm without its own fail-closed checker fails here first. The Bash
// counterparts assert the shared reader was NOT collaterally tightened — bash
// genuinely does de-escape `g\it` to git and does treat /dev/null as the null
// device, and reviewers depend on those.
#[test]
fn read_only_powershell_tool_is_denied_wholesale() {
    let cwd = "C:\\github\\Personal\\TermAl";
    let allow = |tool: &str, command: &str| {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_permission_request(tool, json!({ "command": command })),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            cwd,
        )
        .unwrap()
        .expect("permission request should be classified");
        matches!(
            action,
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. })
        )
    };

    for command in [
        // Every historical bypass shape.
        "echo (Set-Content victim.txt data)",
        "echo @(Set-Content victim.txt data)",
        "cd (Set-Content victim.txt data)",
        "g\\it status",
        "git status 2>/dev/null",
        "git status 2> /dev/null",
        // ...and the innocuous reads the arm used to clear: denial is wholesale, so
        // nothing here is a judgement call the parser can get wrong.
        "git --no-optional-locks status --short",
        "git --no-pager diff --cached --name-only",
        "echo hello",
        "cd \"C:/github/Personal/TermAl\" && git status",
    ] {
        assert!(
            !allow("PowerShell", command),
            "PowerShell must be denied wholesale for read-only reviewers: {command}"
        );
    }

    // The Bash reader keeps its exact behaviour — this fix must not be collateral.
    assert!(allow("Bash", "git --no-optional-locks status --short"));
    assert!(
        allow("Bash", "g\\it status"),
        "bash really does de-escape g\\it to git; the shared reader must still match bash"
    );
    assert!(
        allow("Bash", "git status 2>/dev/null"),
        "/dev/null is the real null device under bash; the idiom must survive"
    );
}

// Pins read-only Claude reviewer delegations denying explicit file mutation
// tool requests. This closes the bug where read-only reviewers used full
// `AutoApprove` and could allow `Write`/`Edit` operations.
#[test]
fn claude_read_only_auto_approve_denies_write_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Write",
            json!({
                "file_path": "src/main.rs",
                "content": "mutated"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        "C:/reviewer-sandbox",
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
        request_id,
        message,
    }) = action
    else {
        panic!("write permission should be denied");
    };

    assert_eq!(request_id, "permission-request-1");
    assert!(message.contains("read-only"));
}

#[test]
fn claude_read_only_auto_approve_denies_unsafe_bash_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Bash",
            json!({
                "command": "echo mutated > README.md"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        "C:/reviewer-sandbox",
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
        request_id,
        message,
    }) = action
    else {
        panic!("unsafe bash permission should be denied");
    };

    assert_eq!(request_id, "permission-request-1");
    assert!(message.contains("read-only"));
}

#[test]
fn claude_read_only_auto_approve_denies_mutating_git_find_and_sed_shapes() {
    for command in [
        "git branch -D old-branch",
        "git branch -m old-name new-name",
        "git branch new-branch",
        "git branch -dfoo",
        "git branch -mNEW",
        "git branch -uorigin/main",
        "git -C /abs branch -uorigin/main",
        // Clustered short options: git parses this as `-q -u origin/main`.
        "git branch -quorigin/main",
        "git branch -qd old-branch",
        "git branch -f",
        // Unambiguous long-option abbreviations git expands to mutating forms.
        "git branch --uns",
        "git branch --edi",
        "git branch --set-up=origin/main",
        // Repository retargeting: a tracked `fixture.git/config` is committable,
        // so these let reviewed content supply `core.fsmonitor`/`diff.external`.
        "git --git-dir=fixture.git --work-tree=. status",
        "git --git-dir=/abs/.git --work-tree=/abs status",
        "git --namespace foo status",
        "git -C /abs diff",
        "git -C \"/path with space\" status",
        "git -C /abs remote -v",
        "git -C /abs ls-files --others --exclude-standard",
        "find . -execdir rm {} \\;",
        "find . -fls files.txt",
        "find . -fprint files.txt",
        "find . -ok rm {} \\;",
        "find . '-execdir' rm {} \\;",
        "sed --in-place s/a/b/ src/main.rs",
        "sed -i.bak s/a/b/ src/main.rs",
        "sed -e w/out.txt src/main.rs",
        "sed '-i.bak' s/a/b/ src/main.rs",
        "sed -e 'w out.txt' src/main.rs",
        "sed -f script.sed src/main.rs",
        "sed 'w/tmp/out' src/main.rs",
        "sed '1w/tmp/out' src/main.rs",
        "sed -n '/foo/w/tmp/out' src/main.rs",
        "sed -e 's/a/b/w out.txt' src/main.rs",
        "sed -e 'W out.txt' src/main.rs",
        "sed -e 'e date' src/main.rs",
        "git diff --output=out.patch",
        "git log --output out.log",
        "git show --output=out.patch HEAD",
        "git diff --ext-diff",
        "git diff --textconv",
        "git -C /abs diff --ext-diff",
        // Abbreviations git expands to the denied options above.
        "git diff --ext",
        "git diff --textc",
        "git diff --out=/tmp/x",
        "git log --outp out.log",
        "git grep --open pattern",
        "git grep -Ocat pattern",
        "git grep -O pattern",
        "git grep --open-files-in-pager pattern",
        // Clustered short options bundle `-O`: git parses `-nOcat` as
        // `-n --open-files-in-pager=cat`.
        "git grep -nOcat pattern",
        "git grep -inOcat pattern",
        "git grep --textconv pattern",
        "git grep --textc pattern",
        // Backslash escaping: bash strips the backslash, so these execute the
        // denied option even though the raw token does not match it literally.
        "git diff --out\\put=/tmp/x",
        "git grep --open\\-files-in-pager=cat pattern",
        // `shortlog --output <path>` writes/truncates an arbitrary file.
        "git shortlog --output=/tmp/x",
        "git shortlog --output /tmp/x HEAD",
        // Shell expansion / subshells the tokenizer cannot resolve rewrite the
        // argv before git runs it, so the literal text must fail closed.
        "git diff $'--outp\\x75t=out.patch'",
        "git diff ${OUT}",
        "git diff $(printf -- --output=x)",
        "git diff `printf x`",
        "git diff --outp{u,X}t=/tmp/x",
        "git diff <(printf x)",
        "git diff \"$OUT\"",
        // Unquoted globs expand before git runs; a tracked filename like
        // `--output=x` turns `git diff *` into a file write.
        "git diff *",
        "git diff *.rs",
        "git diff ?.rs",
        "git diff src/foo[12].rs",
        "git grep pattern *.ts",
        "git branch --set-upstream-to=origin/main",
        "git branch --unset-upstream",
        "git branch --edit-description",
        "git branch --create-reflog",
        "git -C /abs commit -m x",
        "git -C /abs push",
        "git -c a=b push",
        "git -c diff.external=/x diff",
        "git -c core.fsmonitor=/x status",
        "git -c core.pager=cat diff",
        "git -C /abs checkout .",
        "git -C /abs reset --hard",
        "git -C /abs add .",
        "git add .",
        "git -C /abs restore .",
        "git -C /abs merge main",
        "git -C /abs rebase main",
        "git -C /abs config user.email evil@example.com",
        "git stash",
        "git tag v1.0.0",
        "git switch -c topic",
        "git cherry-pick HEAD~1",
        "git revert HEAD",
        "git clean -fd",
        "git rm -r src",
        "git mv a b",
        "git am patch.mbox",
        "git remote add origin https://example.com/x.git",
        "git remote set-url origin git@example.com:x.git",
        "git remote remove origin",
        "git -C /abs remote prune origin",
        "git -C /abs diff --output=/tmp/x",
        "git --exec-path=/evil diff",
        "git --paginate log",
        "git -p log",
        "git -Z diff",
        "git -C /abs",
        "rg --pre 'cat' pattern src",
        "cat README.md & touch /tmp/termal-owned",
        // An escaped quote must not hide the `&` background separator: bash
        // reads `\"` as a literal, so the trailing `& touch ...` still runs.
        "echo \\\"& touch /tmp/termal-owned",
        // A stripped `2>/dev/null` must not make a real background `&` look
        // escaped: the separator scan runs on the original command.
        "echo first \\2>/dev/null& touch /tmp/termal-owned",
        // `cd` into a repo fixture retargets git the same way `-C`/`--git-dir`
        // do, so a directory change combined with git fails closed.
        "cd fixture.git && git status",
        "cd /tmp && git diff --stat",
        // `--help` / `-h` dispatch through `git help`'s configured viewer.
        "git blame --help",
        "git shortlog --help",
        "git diff --help",
        "git status -h",
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_permission_request("Bash", json!({ "command": command })),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            "C:/reviewer-sandbox",
        )
        .unwrap()
        .expect("permission request should be classified");

        let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
            request_id,
            message,
        }) = action
        else {
            panic!("mutating read-only-looking command should be denied: {command}");
        };

        assert_eq!(request_id, "permission-request-1");
        assert!(message.contains("read-only"));
    }
}

// Pins `clear_claude_turn_state` zeroing every field of `ClaudeTurnState` —
// approval keys, parallel agent group key and order, pending tools, the
// streamed text buffer, the `saw_text_delta` flag, and
// `permission_denied_this_turn`. Guards against leaking per-turn state
// (stale pending tools, phantom parallel agents, already-seen approvals)
// into the next Claude turn, which would corrupt the next transcript.
#[test]
fn clear_claude_turn_state_resets_all_fields() {
    let mut state = ClaudeTurnState {
        approval_keys_this_turn: HashSet::from(["approval-1".to_owned()]),
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["agent-1".to_owned()],
        parallel_agents: HashMap::from([(
            "agent-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Working".to_owned()),
                id: "agent-1".to_owned(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Agent 1".to_owned(),
            },
        )]),
        permission_denied_this_turn: true,
        pending_tools: HashMap::from([(
            "tool-1".to_owned(),
            ClaudeToolUse {
                command: Some("echo hi".to_owned()),
                description: Some("Shell".to_owned()),
                file_path: Some("README.md".to_owned()),
                name: "bash".to_owned(),
                subagent_type: Some("worker".to_owned()),
            },
        )]),
        streamed_assistant_text: "partial".to_owned(),
        saw_text_delta: true,
    };

    clear_claude_turn_state(&mut state);

    assert!(state.approval_keys_this_turn.is_empty());
    assert_eq!(state.parallel_agent_group_key, None);
    assert!(state.parallel_agent_order.is_empty());
    assert!(state.parallel_agents.is_empty());
    assert!(!state.permission_denied_this_turn);
    assert!(state.pending_tools.is_empty());
    assert!(state.streamed_assistant_text.is_empty());
    assert!(!state.saw_text_delta);
}

// Pins `reset_claude_turn_state` as the softer variant used at end-of-turn:
// it runs the full `clear_claude_turn_state` field wipe plus finalizes any
// open streaming text bubble on the recorder and calls `reset_turn_state`.
// Guards against a result envelope leaving a half-streamed text bubble open
// or failing to notify the recorder that the turn has ended, which would
// leak partial text into the next turn's transcript.
#[test]
fn reset_claude_turn_state_clears_all_fields_and_finishes_streaming_text() {
    let mut state = ClaudeTurnState {
        approval_keys_this_turn: HashSet::from(["approval-1".to_owned()]),
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["agent-1".to_owned()],
        parallel_agents: HashMap::from([(
            "agent-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Working".to_owned()),
                id: "agent-1".to_owned(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Agent 1".to_owned(),
            },
        )]),
        permission_denied_this_turn: true,
        pending_tools: HashMap::from([(
            "tool-1".to_owned(),
            ClaudeToolUse {
                command: Some("echo hi".to_owned()),
                description: Some("Shell".to_owned()),
                file_path: Some("README.md".to_owned()),
                name: "bash".to_owned(),
                subagent_type: Some("worker".to_owned()),
            },
        )]),
        streamed_assistant_text: "partial".to_owned(),
        saw_text_delta: true,
    };
    let mut recorder = TestRecorder {
        streaming_text_delta_start: Some(2),
        streaming_text_active: true,
        ..TestRecorder::default()
    };

    reset_claude_turn_state(&mut state, &mut recorder).unwrap();

    assert!(state.approval_keys_this_turn.is_empty());
    assert_eq!(state.parallel_agent_group_key, None);
    assert!(state.parallel_agent_order.is_empty());
    assert!(state.parallel_agents.is_empty());
    assert!(!state.permission_denied_this_turn);
    assert!(state.pending_tools.is_empty());
    assert!(state.streamed_assistant_text.is_empty());
    assert!(!state.saw_text_delta);
    assert_eq!(recorder.reset_turn_state_calls, 1);
    assert_eq!(recorder.finish_streaming_text_calls, 2);
    assert_eq!(recorder.streaming_text_delta_start, None);
    assert!(!recorder.streaming_text_active);
}

// Pins `handle_claude_tool_use` fanning out two concurrent `task` tool_use
// frames into a pair of `ParallelAgentProgress` entries titled by
// `description`, both in `Initializing` status with detail "Initializing...".
// Guards against the `task` fan-out being lost, collapsed into a single
// agent, or recorded with the wrong status so the UI would show only one
// sub-agent instead of the full group running in parallel.
#[test]
fn claude_task_tool_use_updates_parallel_agent_progress() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    },
                    {
                        "type": "tool_use",
                        "id": "task-2",
                        "name": "Task",
                        "input": {
                            "description": "Architecture code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("parallel agents update should be recorded");
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].detail.as_deref(), Some("Initializing..."));
    assert_eq!(latest[0].status, ParallelAgentStatus::Initializing);
    assert_eq!(latest[1].title, "Architecture code review");
    assert_eq!(latest[1].status, ParallelAgentStatus::Initializing);
}

// Pins `handle_claude_task_tool_result` advancing an initializing
// `ParallelAgentProgress` to `Completed` with a single-line detail preview,
// and emitting a `push_subagent_result` carrying the full multi-line body.
// Guards against the parent transcript losing the sub-agent's return value
// or the progress card being stuck in `Initializing` after the task tool
// returns successfully.
#[test]
fn claude_task_tool_result_updates_parallel_agents_and_records_subagent_result() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer found a batching bug in location smoothing.\nRead src/state.rs for the stale preview path.";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("completed parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].source, ParallelAgentSource::Tool);
    assert_eq!(latest[0].status, ParallelAgentStatus::Completed);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer found a batching bug in location smoothing.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

// Pins Claude task-result updates reclaiming an existing progress row as
// tool-sourced. This is a release-mode guard: a source mismatch must not
// silently preserve a delegation-routable id on a Claude Task row.
#[test]
fn claude_task_tool_result_resets_existing_non_tool_progress_source() {
    let mut turn_state = ClaudeTurnState {
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["task-1".to_owned()],
        parallel_agents: HashMap::from([(
            "task-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Running".to_owned()),
                id: "task-1".to_owned(),
                source: ParallelAgentSource::Delegation,
                status: ParallelAgentStatus::Running,
                title: "Task agent".to_owned(),
            },
        )]),
        pending_tools: HashMap::from([(
            "task-1".to_owned(),
            ClaudeToolUse {
                command: None,
                description: Some("Rust code review".to_owned()),
                file_path: None,
                name: "Task".to_owned(),
                subagent_type: Some("general-purpose".to_owned()),
            },
        )]),
        ..ClaudeTurnState::default()
    };
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": "Reviewer finished."
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("parallel agent source repair should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].id, "task-1");
    assert_eq!(latest[0].source, ParallelAgentSource::Tool);
    assert_eq!(latest[0].status, ParallelAgentStatus::Completed);
    assert_eq!(
        turn_state
            .parallel_agents
            .get("task-1")
            .expect("task row should remain")
            .source,
        ParallelAgentSource::Tool,
    );
}

// Pins `handle_claude_task_tool_error` flipping the progress entry to
// `Error` with the first failure line as the preview detail, while handing
// the full multi-line payload (stack trace and all) to the recorder via
// `push_subagent_result`. Guards against failure diagnostics being
// truncated to the preview or dropped entirely, which would hide the real
// cause of the sub-agent failure from the user.
#[test]
fn claude_task_tool_error_records_full_failure_detail() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer failed to parse the diff.\nStack trace line 1\nStack trace line 2";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer failed to parse the diff.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

// Pins `handle_claude_task_tool_error` substituting the literal "Task
// failed." string when the tool_result has `is_error: true` but an empty
// content body — used both for the progress detail and for
// `push_subagent_result`. Guards against empty-detail errors producing an
// empty subagent result bubble or a parallel agent card that shows no
// reason for the failure.
#[test]
fn claude_task_tool_error_without_detail_records_fallback_failure_message() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": ""
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(latest[0].detail.as_deref(), Some("Task failed."));
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), "Task failed.".to_owned())]
    );
}

// Pins `handle_claude_streamed_text` reconciling a short stream ("Hello")
// with a longer final assistant text ("Hello there.") arriving after
// `message_stop`, by appending the missing " there." suffix to the open
// bubble so the transcript ends up with the full final text in a single
// `Message::Text`. Guards against lost trailing words when Claude flushes
// the full payload only in the post-`message_stop` `assistant` envelope.
#[test]
fn claude_streamed_text_appends_missing_final_suffix_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Pins `handle_claude_streamed_text` recognizing that the final assistant
// text exactly matches the already-streamed buffer and skipping the append,
// so the transcript keeps a single `Message::Text` rather than duplicating
// the full line. Guards against doubled assistant text in the bubble when
// Claude's post-`message_stop` payload restates the complete streamed body
// verbatim.
#[test]
fn claude_streamed_text_skips_duplicate_final_text_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello there."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Pins `handle_claude_streamed_text` calling `replace_streaming_text` when
// the final assistant body ("Final answer.") is not a prefix-extension of
// the streamed draft ("Draft answer."), so the bubble is rewritten in
// place to the authoritative final text. Guards against TermAl keeping a
// stale early draft (or concatenating draft+final) when Claude rewrites
// its own in-flight text.
#[test]
fn claude_streamed_text_replaces_divergent_final_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Draft answer."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Final answer."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final answer."
    ));
}

// Pins the transcript boundary: `handle_claude_tool_use` arriving after a
// streamed text bubble has ended must close the text `Message` and start a
// fresh `Message::Command`, then a subsequent stream delta opens yet
// another text bubble — yielding three distinct messages (text, command,
// text) in order. Guards against follow-up tool calls or post-tool text
// being appended to an already-closed text bubble.
#[test]
fn claude_tool_use_after_streamed_text_starts_followup_in_new_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "World"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 3);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Command {
            command,
            output,
            status,
            ..
        }) if command == "pwd" && output.is_empty() && *status == CommandStatus::Running
    ));
    assert!(matches!(
        session.messages.get(2),
        Some(Message::Text { text, .. }) if text == "World"
    ));
}

// Pins `handle_claude_result` draining `pending_tools` so that a
// tool_result envelope arriving after the turn's `result` is silently
// discarded rather than mutating a recorded command — the Running Bash
// command keeps its original empty output and `Running` status. Guards
// against stray late tool-result frames from Claude retroactively
// rewriting a completed turn's transcript.
#[test]
fn claude_result_clears_pending_tools_and_ignores_late_tool_results() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert!(turn_state.pending_tools.is_empty());

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "bash-1",
                        "content": "/tmp/late"
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.commands,
        vec![("pwd".to_owned(), String::new(), CommandStatus::Running)]
    );
}

// Pins `handle_claude_result` resetting the recorder's command-id keying
// between turns so a second turn reusing the same `tool_use_id` ("bash-1")
// registers a fresh command rather than overwriting the prior turn's
// completed Bash message — both commands end up persisted with their own
// output and `Success` status. Guards against cross-turn id collisions
// merging two independent Bash invocations into one transcript entry.
#[test]
fn claude_result_resets_recorder_command_keys_between_turns() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    for (command, output) in [("pwd", "/tmp/one"), ("git status", "working tree clean")] {
        handle_claude_event(
            &json!({
                "type": "assistant",
                "message": {
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "bash-1",
                            "name": "Bash",
                            "input": {
                                "command": command
                            }
                        }
                    ]
                }
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
        handle_claude_event(
            &json!({
                "type": "user",
                "message": {
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "bash-1",
                            "content": output
                        }
                    ]
                }
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
        handle_claude_event(
            &json!({
                "type": "result",
                "is_error": false
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");
    let commands = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        commands,
        vec![
            (
                "pwd".to_owned(),
                "/tmp/one".to_owned(),
                CommandStatus::Success
            ),
            (
                "git status".to_owned(),
                "working tree clean".to_owned(),
                CommandStatus::Success
            ),
        ]
    );
}
