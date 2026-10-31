---
name: review-code
description: Inspect working-tree changes or a recorded commit against its base using specialized reviewer lenses.
metadata:
  termal:
    title:
      strategy: default
---

Inspect staged, unstaged and untracked changes, or a supplied committed input
against its recorded base, using multiple specialized reviewer lenses.

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. Read-only git commands (`diff`, `status`, `ls-files`, `show`, etc.) may be executed freely. Do not run mutating git commands (`add`, `stash`, `checkout`, reset operations, etc.) as part of this command.**

**IMPORTANT: `/review-code` is inspection-only in both direct and delegated sessions. Do NOT attempt to fix bugs, edit files, run mutating Git commands, mutate the tracker, or inspect tracker tasks to reconcile review findings. Read-only startup recovery required by project instructions is allowed. Report findings and proposed tracker follow-ups for the parent `/review-changes` coordinator without changing the workspace or tracker. These proposals are not tracker updates and may duplicate existing issues; only `/review-changes` reconciles them with the Engram tracker after consolidating all reviewer results.**

**IMPORTANT: Do NOT run compilation, build, test, type-check, lint, benchmark, coverage, or formatting gates from `/review-code`, even when a command appears read-only. This includes `cargo check`, `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`, TypeScript compilers, Vitest, ESLint, Prettier, and package-manager test/build scripts. The parent `/review-changes` session exclusively owns quality gates. Reviewers may inspect existing source, tests, configuration, diffs, and previously produced output.**

## Step 1: Get the changes

For a supplied committed input, require the parent's recorded full base
commit (`BASE`), candidate commit (`COMMIT`) and candidate tree id (`TREE`).
Missing or invalid identities in committed-input mode are unavailable
coverage; report them and stop rather than fall back to working-tree changes
or infer a base from `HEAD^`.

For this mode, at both the start and end of the review, run separately:

```bash
git rev-parse HEAD
git show -s --format=%T COMMIT
git status --porcelain --untracked-files=all
```

Require zero exit statuses, `HEAD` equal to the recorded `COMMIT`, the tree
equal to `TREE`, and an empty status listing. Verify that `BASE` and `COMMIT`
are the recorded commit objects. Report the three ids and both boundary
results. A dirty checkout or identity mismatch is unavailable coverage, not
a clean review. A known intervening source or index edit invalidates the
affected result even if the final checks match.

For a supplied committed input, read its complete change against the supplied
base, including added and deleted files, even though the checkout is clean:

```bash
git diff --name-only BASE COMMIT --
git diff BASE COMMIT --
```

Require both inspection commands to succeed; report a refused or unreadable
diff as a coverage gap, never as an empty change.
Read relevant file content from `COMMIT`, or from `BASE` for a deleted file.
Untracked files are absent in this mode. If the supplied range is empty,
report that there is nothing to review and stop. These read-only target
checks grant no Git mutation or gate authority.

Without a supplied committed input, run `git status --short`, `git diff`,
`git diff --cached`, and `git ls-files --others --exclude-standard` to get all
staged, unstaged and untracked changes. If there are none, report that and stop.
Also run `git diff --name-only` and `git diff --cached --name-only` to list
changed tracked files. Include the untracked list because those files do not
appear in `git diff`; inspect their contents directly on demand.
Do NOT read full file contents upfront — read files on demand in either mode.

On Windows, a read-only Codex reviewer reads source this way (TermAl also appends it to such a delegated child's prompt):

Windows read-only Codex inspection: use the default shell; do not request bash or change sandbox, policy, or encoding settings. Read source through direct native Git or rg output, one inspection command per call, without piping that output through PowerShell cmdlets. For an index blob use git --no-pager -C <worktree> show :path; for a frozen blob use git --no-pager -C <worktree> show <tree-or-commit>:path. These show the selected Git object, not unstaged working-tree changes. For a current file use git --no-pager -C <worktree> diff --no-index --no-ext-diff --no-textconv --color=never -- NUL <absolute-file>, or rg --encoding utf-8 --line-number --context 12 -e <pattern> -- <absolute-file>. Quote paths and patterns for the default shell. A no-index diff normally exits 1 when it displays the file; rg exits 1 for no match. Inspect stderr and coverage instead of equating these statuses with a read failure. Do not use Get-Content (including -Encoding UTF8), Select-Object, Out-String or other PowerShell pipeline rendering to inspect source whose exact text matters. ConstrainedLanguage may refuse .NET constructors, static methods and Console.OutputEncoding assignments; nonterminating errors can still leave exit code 0, and a swallowed encoding error may be invisible. Re-read affected text with a native form. If exact text or complete coverage cannot be obtained, report inspection unavailable rather than a clean review.

## Step 2: Discover reviewers

Run `find .claude/reviewers -name "*.md" 2>/dev/null` via Bash to find all available reviewer lens files.
(Do NOT use the Glob tool here — it silently fails on Windows paths.)
Read each file to get:
- The reviewer name (from the filename, e.g., `rust.md` → "Rust")
- The review instructions (the file content)

## Step 3: Run reviewers

If this session prompt contains `You are a delegated child session for TermAl delegation`,
run reviewer lenses inline only. In that delegated-child mode, do not use Task tool calls or TermAl delegation MCP tools. Do not spawn reviewers through Claude
Task agents, Codex subagents, shell-launched agents, TermAl nested delegations
via `termal_spawn_session`, raw HTTP reviewers, or nested review commands. The
disallowed nested paths are Claude Task agents, Codex subagents, shell-launched agents, TermAl nested delegations via `termal_spawn_session`,
raw HTTP reviewers, and nested review commands. In the final summary, state
that nested reviewer spawning was intentionally skipped.

Run each reviewer lens in this same session using the prompt below as the lens checklist. Do not launch TermAl delegations, Claude Task agents, Codex subagents, shell-launched agents, raw HTTP reviewers, or nested review commands from `/review-code`; use `/review-changes` for cross-agent delegated review.

For each reviewer found in Step 3, use this prompt:

```
You are a code reviewer focusing on: [REVIEWER NAME]

This is a review-only task. Do NOT attempt to fix any bugs or edit any files.
Your job is to identify issues and propose follow-up work for the parent
`/review-changes` coordinator to evaluate after consolidating all reviewer
results. Read-only startup recovery required by project instructions is allowed. Do not run quality gates, mutate the tracker, or inspect tracker tasks to reconcile review findings;
review the code.

## Your Review Instructions
[CONTENT OF THE REVIEWER .md FILE]

## Changes to Review
[The complete BASE-to-COMMIT diff for a committed input; otherwise the working-tree diffs]

## Changed Files
[The selected target's changed paths, including additions and deletions — read files on demand for context]

## Untracked Files
[None for a clean committed input; otherwise the untracked list, with contents read on demand]

## Project Context
This is TermAl — a WhatsApp-style control room for managing AI coding agents locally.
- Backend: Rust (Axum + Tokio), with modules under `src/` and `src/main.rs` as the entrypoint
- Frontend: React 18 + TypeScript (Vite), with `ui/src/App.tsx` plus split feature modules
- Agent integration: Claude Code (NDJSON/stdio), Codex (JSON-RPC/stdio), Gemini/Cursor (ACP)
- Real-time: SSE with monotonic revision counter, delta events for streaming
- State: Arc<Mutex<StateInner>> with commit_locked() pattern
- Persistence: embedded SQLite at `~/.termal/termal.sqlite` for app state, sessions, and delegations; `~/.termal/orchestrators.json` for reusable orchestrator templates; `~/.termal/telegram-bot.json` for optional Telegram relay metadata/state
- Workspace: binary tree of panes with draggable tabs
- Styling: custom CSS with CSS variables, 17 themes (no Tailwind)
- Testing: Rust `cargo test`; frontend Vitest + React Testing Library
- Local-only: no auth and no cloud sync; SQLite is embedded, not a database server
Read docs/architecture.md or relevant source files for deeper context if needed.

## Known Accepted Patterns (do NOT flag these)
- Large-file cleanup is ongoing. Do not flag untouched legacy files solely for size, but do flag reviewed files that exceed the active architecture threshold when the issue is not already tracked in Engram.
- `expect("state mutex poisoned")` on mutex locks — project convention
- `std::thread::spawn` for agent runtime threads (intentional — blocking stdio)
- `0.0.0.0` binding on the HTTP server (configurable, documented as local-only)
- No authentication (Phase 1 is single-user local)
- Do NOT require backward compatibility for obsolete local-only development persistence schemas, `~/.termal/sessions.json`, browser localStorage state, or local/internal API contracts unless current code/docs explicitly promise a migration.
- Path normalization and canonicalization for current inputs are not legacy compatibility work.
- Windows, macOS, and Linux are P0 platforms. Flag regressions on those platforms; do not require support beyond them unless the current change claims it.
- Agent-specific protocol differences (Claude=NDJSON, Codex=JSON-RPC) — by design

## Output Format
Return your findings as a structured list:

### [REVIEWER NAME] Review

**Findings:**

For each issue found:
- **[SEVERITY: Critical/High/Medium/Low/Note]** `file:line` — Description of the issue
  - Why it matters: [explanation]
  - Suggested fix direction: [if applicable; do not implement it]

If no issues found, say "No issues found."

**Summary:** [1-2 sentence overall assessment]
```

## Step 4: Consolidate

After all reviewers complete, merge their findings into a single review note:

For a committed input, include the recorded `BASE`, `COMMIT` and `TREE` and
the independent start/end identity results with the reported coverage.

```markdown
# Code Review — [date]

## Changes Reviewed
- [list of changed files]

## Actionable
[Findings that require code changes, grouped by severity: Critical → High → Medium → Low]

## Informational
[Observations, style notes, and FYI items that don't require immediate action]

## Reviewer Summaries
- **Architecture**: [summary]
- **Rust**: [summary]
- **React/TypeScript**: [summary]
- **Security**: [summary]
- **Testing**: [summary]
- **API**: [summary]
```

Deduplicate: if two reviewers flag the same issue, merge them (note which reviewers caught it).

Present the consolidated note directly to the user. Do NOT write the review note to a separate file.

## Step 5: Propose tracker follow-ups

Read-only startup recovery required by project instructions is allowed; all tracker reconciliation and mutations belong to the parent. If the review identifies an actionable issue, test gap, resolved issue, or follow-up, include a `Suggested tracker updates` section describing what the parent `/review-changes` session should evaluate for creation, update, comment, or closure. Include the proposed issue type and priority for possible new work, but label every item as a proposal that may already be tracked.

If the review is clean, say `No tracker follow-up suggested.` Do not claim that the tracker is up to date, that an issue was created or updated, or that a specific existing issue is the correct target because `/review-code` does not reconcile findings with tracker tasks. The parent `/review-changes` workflow owns consolidation, deduplication, tracker lookup, and all tracker mutations.
