---
name: review-code
description: Inspect staged, unstaged, and untracked changes using multiple specialized reviewer lenses.
metadata:
  termal:
    title:
      strategy: default
---

Inspect staged, unstaged, and untracked changes using multiple specialized reviewer lenses.

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. Read-only git commands (`diff`, `status`, `ls-files`, `show`, etc.) may be executed freely. Do not run mutating git commands (`add`, `stash`, `checkout`, reset operations, etc.) as part of this command.**

**IMPORTANT: `/review-code` is inspection-only in both direct and delegated sessions. Do NOT attempt to fix bugs, edit files, run mutating Git commands, inspect the existing Beads tracker, or call `bd`. Report findings and proposed tracker follow-ups for the parent `/review-changes` coordinator without changing the workspace or tracker. These proposals are not tracker updates and may duplicate existing issues; only `/review-changes` reconciles them with Beads after consolidating all reviewer results.**

**IMPORTANT: Do NOT run compilation, build, test, type-check, lint, benchmark, coverage, or formatting gates from `/review-code`, even when a command appears read-only. This includes `cargo check`, `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`, TypeScript compilers, Vitest, ESLint, Prettier, and package-manager test/build scripts. The parent `/review-changes` session exclusively owns quality gates. Reviewers may inspect existing source, tests, configuration, diffs, and previously produced output.**

## Step 1: Get the changes

Run `git status --short`, `git diff`, `git diff --cached`, and `git ls-files --others --exclude-standard` to get all staged, unstaged, and untracked changes.
If there are no staged, unstaged, or untracked changes, tell the user and stop.

Also run `git diff --name-only` and `git diff --cached --name-only` to get the list of changed tracked files.
For untracked files, include the `git ls-files --others --exclude-standard` list in the reviewer prompt because untracked files do not appear in `git diff`.
Do NOT read full file contents upfront — reviewers will read files on-demand as needed. For untracked files, reviewers must inspect file contents directly on demand.

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
results. Do not inspect the existing tracker, run quality gates, or call `bd`;
inspect code only.

## Your Review Instructions
[CONTENT OF THE REVIEWER .md FILE]

## Changes to Review
[THE GIT DIFF]

## Changed Files
[LIST OF CHANGED FILE PATHS — read files on-demand as needed for context, don't rely on diff alone]

## Untracked Files
[LIST OF UNTRACKED FILE PATHS FROM git ls-files --others --exclude-standard — read files on-demand as needed because they do not appear in the diff]

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
- Large-file cleanup is ongoing. Do not flag untouched legacy files solely for size, but do flag reviewed files that exceed the active architecture threshold when the issue is not already tracked in beads.
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

Do not inspect the existing tracker or run any `bd` command. If the review identifies an actionable issue, test gap, resolved issue, or follow-up, include a `Suggested beads updates` section describing what the parent `/review-changes` session should evaluate for creation, update, comment, or closure. Include the proposed issue type and priority for possible new work, but label every item as a proposal that may already be tracked.

If the review is clean, say `No tracker follow-up suggested.` Do not claim that Beads is up to date, that an issue was created or updated, or that a specific existing issue is the correct target because `/review-code` does not inspect the tracker. The parent `/review-changes` workflow owns consolidation, deduplication, tracker lookup, and all tracker mutations.
