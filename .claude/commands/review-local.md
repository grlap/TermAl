---
name: review-local
description: Review staged and unstaged changes using multiple specialized reviewers.
metadata:
  termal:
    title:
      strategy: default
    delegation:
      enabled: true
      mode: reviewer
      writePolicy:
        kind: isolatedWorktree
---

Review staged and unstaged changes using multiple specialized reviewers.

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. All other git commands (diff, status, stash, add, etc.) may be executed freely.**

**IMPORTANT: This command is review-only. Do NOT attempt to fix any bugs, edit source files, edit tests, run formatters that modify files, or otherwise change implementation code. The only allowed file update is `docs/bugs.md` in Step 6. If the review finds anything of any severity, `docs/bugs.md` MUST be updated in the same run to record or reconcile those findings.**

## Step 1: Build check

Run `cargo check` to ensure the Rust backend compiles.
If it produces ANY errors — **STOP immediately**.
Present the full output to the user and do NOT proceed to further steps.
Warnings are acceptable — report them but continue.

Then run `cd ui && npx tsc --noEmit` to type-check the frontend.
If it produces ANY errors — **STOP immediately**.
Present the full output to the user and do NOT proceed to further steps.

## Step 2: Get the changes

Run `git diff` and `git diff --cached` to get all staged and unstaged changes.
If there are no changes, tell the user and stop.

Also run `git diff --name-only` and `git diff --cached --name-only` to get the list of changed files.
Do NOT read full file contents upfront — subagents will read files on-demand as needed.

## Step 3: Discover reviewers

Run `find .claude/reviewers -name "*.md" 2>/dev/null` via Bash to find all available reviewer lens files.
(Do NOT use the Glob tool here — it silently fails on Windows paths.)
Read each file to get:
- The reviewer name (from the filename, e.g., `rust.md` → "Rust")
- The review instructions (the file content)

## Step 4: Run reviewers in parallel

For each reviewer found in Step 3, launch a **Task agent** (subagent_type: general-purpose) with this prompt:

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

## Project Context
This is TermAl — a WhatsApp-style control room for managing AI coding agents locally.
- Backend: Rust (Axum + Tokio), single file: src/main.rs (~7600 lines)
- Frontend: React 18 + TypeScript (Vite), main file: ui/src/App.tsx (~4500 lines)
- Agent integration: Claude Code (NDJSON/stdio), Codex (JSON-RPC/stdio), Gemini/Cursor (ACP)
- Real-time: SSE with monotonic revision counter, delta events for streaming
- State: Arc<Mutex<StateInner>> with commit_locked() pattern
- Persistence: single JSON file at ~/.termal/sessions.json
- Workspace: binary tree of panes with draggable tabs
- Styling: custom CSS with CSS variables, 17 themes (no Tailwind)
- Testing: Vitest + React Testing Library (frontend only currently)
- No database, no auth, no cloud sync (Phase 1 local-only)
Read docs/architecture.md or relevant source files for deeper context if needed.

## Known Accepted Patterns (do NOT flag these)
- Single large files (main.rs, App.tsx) — intentional tradeoff for iteration speed
- `expect("state mutex poisoned")` on mutex locks — project convention
- `std::thread::spawn` for agent runtime threads (intentional — blocking stdio)
- `0.0.0.0` binding on the HTTP server (configurable, documented as local-only)
- No authentication (Phase 1 is single-user local)
- Legacy compatibility means supporting older persisted schema or older local/internal API shapes from previous development builds, such as obsolete orchestrator fields.
- Do NOT flag missing schema upgrades, migrations, or backward compatibility for ~/.termal/*.json, browser localStorage state, or local/internal API contracts from previous local-only development builds.
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

Run ALL reviewer agents in parallel using multiple Task tool calls in a single message.

## Step 5: Consolidate

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

## Step 6: Update `docs/bugs.md`

After presenting the review to the user, update `docs/bugs.md` to reflect the findings. Do not modify any other file. If any reviewer found any issue, observation, test gap, or note of any severity, `docs/bugs.md` MUST be updated before the command is complete. Read the file first to understand the current structure, then apply these three operations:

### 6a. Remove resolved bugs

If the reviewed changes fix any **active bug entries** (the `## Heading` sections with Severity/Current behavior/Proposal), **delete those entries entirely** from `docs/bugs.md`.

`docs/bugs.md` is an active-state ledger only. Its own preamble explicitly says "Resolved work, fixed-history notes, speculative refactors, cleanup notes, and external limitations do not belong here." Do NOT move resolved entries into a "fixed in the current tree" preamble paragraph or any other history-style note. Just remove them.

If the reader needs to see what changed, that is what `git log` and PR descriptions are for — not `docs/bugs.md`.

### 6b. Add new bug entries

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

### 6c. Add or update task list items

For **test gaps and coverage improvements** identified by the review, add P2 task items to the Implementation Tasks section. Match the existing format:

```markdown
- [ ] Short task description:
  longer explanation spanning one or two lines if needed.
```

Remove any task items that the reviewed changes have completed (e.g., if a test gap was filled, remove that task).

### 6d. Skip if clean

Only skip `docs/bugs.md` when the review found no issues, no observations, no notes, no test gaps, no resolved active bugs, and no completed tasks. Tell the user "bugs.md is up to date — no changes needed."

