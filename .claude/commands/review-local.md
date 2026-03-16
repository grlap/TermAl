Review staged and unstaged changes using multiple specialized reviewers.

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. All other git commands (diff, status, stash, add, etc.) may be executed freely.**

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
- Agent-specific protocol differences (Claude=NDJSON, Codex=JSON-RPC) — by design

## Output Format
Return your findings as a structured list:

### [REVIEWER NAME] Review

**Findings:**

For each issue found:
- **[SEVERITY: Critical/High/Medium/Low/Note]** `file:line` — Description of the issue
  - Why it matters: [explanation]
  - Suggested fix: [if applicable]

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

Present the consolidated note directly to the user (do NOT write it to a file unless asked).
