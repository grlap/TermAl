---
name: fix-bug
description: Fix a bug from beads (bd) by id.
metadata:
  termal:
    title:
      strategy: prefixFirstArgument
      prefix: Fix bug
---

Fix a bug tracked in beads by id (e.g., `tm-qu8`).

Arguments: $ARGUMENTS (required bead id). If omitted, run `bd ready` / `bd list` and ask the user which bug to fix.

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. All other git commands (diff, status, stash, add, etc.) may be executed freely.**

## Step 1: Parse the bug

Run `bd show $ARGUMENTS` to load the bug.

- If the id is not found → tell the user and **stop**.
- If the bug is already closed → tell the user and **stop**.
- Extract: **priority** (P0–P4), **files** mentioned, **description**, and **recommended fix** (if any).
- Present a brief summary to the user: id, priority, one-line title.

## Step 2: Assess the bug (push-back gate)

Read every file referenced in the bug. Examine the actual code and surrounding context.

Evaluate whether the bug is **valid** and whether the **priority is accurate**. Consider:
- Has the bug already been fixed in a later change?
- Is the described behavior actually a bug, or expected/harmless?
- Is the priority too high or too low?

**If you agree** the bug is valid → proceed to Step 3.

**If you disagree** (false positive, wrong priority, already fixed, or not worth fixing) → present your reasoning and offer options:
- Agree with the original assessment and proceed
- Change priority (`bd priority $ARGUMENTS <0-4>`) and proceed
- Close as false positive / already fixed
- Proceed with the fix anyway

If the user chooses to close without fixing:
1. `bd close $ARGUMENTS --reason "<why>"` (e.g. false-positive / already-fixed)
2. Present what was changed and **stop**

## Step 3: Fix the bug

Implement the fix:

1. Read the referenced files and any surrounding code needed for context
2. Follow existing project patterns — check nearby code for conventions:
   - Rust backend (`src/main.rs`): error handling via `ApiError`, state mutation via `commit_locked()`, serde with `camelCase`
   - React frontend (`ui/src/`): hooks, TypeScript types in `types.ts`, API calls in `api.ts`
   - Keep changes consistent with `docs/architecture.md`
3. Keep the fix **minimal and focused** — do not refactor unrelated code
4. If the approach is ambiguous or there are multiple valid solutions → ask the user before writing code

**Related bugs:** If 2–3 other open bugs share the same root cause or touch the same file (check `bd list` / `bd ready`), mention them to the user and offer to fix them together in one pass. Do not batch more than 3 bugs.

## Step 4: Verify the fix

Run these checks sequentially:

1. `cargo check` — if errors appear, fix them and re-run
2. `cd ui && npx tsc --noEmit` — if TypeScript errors appear, fix them and re-run
3. `cd ui && npx vitest run` — if tests fail due to the fix, update or add tests as needed
   - If tests fail for unrelated reasons, note it but continue

All checks must pass before proceeding.

## Step 5: Review via /review-changes

Invoke `/review-changes` directly in the active parent session to run validation and get independent Codex and Claude `/review-code` sign-off on the changes.

After the review completes:
- **Critical or High findings** → fix them, re-run Step 4, and re-review
- **Medium or Low findings** → present to the user; proceed if they accept
- **No findings** → proceed

## Step 6: Close the bug in beads

Once the fix is verified and reviewed:

1. Close the issue: `bd close $ARGUMENTS` (add `--reason "<summary>"` if useful)
2. Present a final summary:
   - Bead id and title
   - What was changed (files modified)
   - How it was verified (checks, tests, review)
   - Any related bugs the user may want to address next
