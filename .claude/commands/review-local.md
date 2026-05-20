---
name: review-local
description: Review staged, unstaged, and untracked changes using multiple specialized reviewers.
metadata:
  termal:
    title:
      strategy: default
---

Review staged, unstaged, and untracked changes using multiple specialized reviewers.

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. Read-only git commands (`diff`, `status`, `ls-files`, `show`, etc.) may be executed freely. Do not run mutating git commands (`add`, `stash`, `checkout`, reset operations, etc.) as part of this command.**

**IMPORTANT: This command is review-only. Do NOT attempt to fix any bugs, edit source files, edit tests, run formatters that modify files, or otherwise change implementation code. The only allowed file update is `docs/bugs.md` in Step 5.**

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

Run each reviewer lens in this same session using the prompt below as the lens checklist. Do not launch TermAl delegations, Claude Task agents, Codex subagents, shell-launched agents, raw HTTP reviewers, or nested review commands from `/review-local`; use `/review-with-delegate` for cross-agent delegated review.

For each reviewer found in Step 3, use this prompt:

```
You are a code reviewer focusing on: [REVIEWER NAME]

This is a review-only task. Do NOT attempt to fix any bugs or edit any files.
Your job is to identify issues and propose follow-up work for the main reviewer
to record in docs/bugs.md.

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
- Large-file cleanup is ongoing. Do not flag untouched legacy files solely for size, but do flag reviewed files that exceed the active architecture threshold when the issue is not already tracked in `docs/bugs.md`.
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

## Step 5: Update `docs/bugs.md`

After presenting the review to the user, update `docs/bugs.md` to reflect the findings. Do not modify any other file. If file edits are unavailable under the active session policy, include a "Suggested `docs/bugs.md` updates" section with the entries/tasks/removals that should be applied. If any reviewer found any issue, observation, test gap, or note of any severity, `docs/bugs.md` MUST be updated before the command is complete. Read the file first to understand the current structure, then apply these three operations:

### 5a. Remove resolved bugs

If the reviewed changes fix any **active bug entries** (the `## Heading` sections with Severity/Current behavior/Proposal), **delete those entries entirely** from `docs/bugs.md`.

`docs/bugs.md` is an active-state ledger only. Its own preamble explicitly says "Resolved work, fixed-history notes, speculative refactors, cleanup notes, and external limitations do not belong here." Do NOT move resolved entries into a "fixed in the current tree" preamble paragraph or any other history-style note. Just remove them.

If the reader needs to see what changed, that is what `git log` and PR descriptions are for — not `docs/bugs.md`.

### 5b. Add new bug entries

For each finding from the review (any severity: Critical, High, Medium, Low, or Note) that is NOT already tracked in bugs.md, add a new bug entry in the active section (between the preamble and the first existing bug entry, or wherever severity-ordering fits). Use the existing format:

```markdown
## Short description of the issue

**Severity:** [Critical/High/Medium/Low/Note] - brief impact summary.

[1-2 paragraph explanation of the problem.]

**Current behavior:**
- [bullet points describing what happens now]

**Proposal:**
- [bullet points describing the recommended fix]
```

If a finding is already tracked in `docs/bugs.md`, do not create a duplicate. Update the existing bug entry or task item as needed to reflect the current review evidence, affected files, severity, or proposal. Do not leave `docs/bugs.md` unchanged when the review found something.

### 5c. Add or update task list items

For **test gaps and coverage improvements** identified by the review, add P2 task items to the Implementation Tasks section. Match the existing format:

```markdown
- [ ] Short task description:
  longer explanation spanning one or two lines if needed.
```

Remove any task items that the reviewed changes have completed (e.g., if a test gap was filled, remove that task).

### 5d. Skip if clean

Only skip `docs/bugs.md` when the review found no issues, no observations, no notes, no test gaps, no resolved active bugs, and no completed tasks. Tell the user "bugs.md is up to date — no changes needed."
