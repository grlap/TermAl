---
name: review-with-delegate
description: Review current changes by running /review-local in both Codex and Claude TermAl delegations.
metadata:
  termal:
    title:
      strategy: default
---

Review current staged and unstaged changes by delegating `/review-local` to both Codex and Claude through TermAl delegation sessions.

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. Read-only git commands (`diff`, `status`, `ls-files`, `show`, etc.) may be executed freely. Mutating git commands (`add`, `stash`, `checkout`, reset operations, etc.) may only be used when the current session write policy allows workspace mutation.**

**IMPORTANT: This command must use TermAl delegations to create the two top-level reviewer sessions. Do NOT use raw `claude -p`, Codex platform subagents, Claude Task agents, or any non-TermAl review path to spawn those top-level reviewers. The delegated child sessions may execute `/review-local` using that command's own documented internal review workflow. If TermAl delegation tools are unavailable, stop and report that `/review-with-delegate` requires the TermAl delegation tool surface.**

## Step 1: Confirm review target

Run `git status --short`, `git diff --name-only`, `git diff --cached --name-only`, and `git ls-files --others --exclude-standard`.

If there are no staged, unstaged, or untracked changes, tell the user there is nothing to review and stop.

## Step 2: Spawn delegated reviewers

Using TermAl's delegation tool surface, create two child delegation sessions from the current parent session:

1. Codex reviewer
   - Agent: `Codex`
   - Prompt: `/review-local`
   - Mode: `reviewer`
   - Write policy: `readOnly`.
   - Title: `Codex /review-local`

2. Claude reviewer
   - Agent: `Claude`
   - Prompt: `/review-local`
   - Mode: `reviewer`
   - Write policy: `readOnly`.
   - Title: `Claude /review-local`

Use read-only delegation sessions here so reviewers see the exact current worktree, including untracked files. Do not request `isolatedWorktree` for this command until the tracked `docs/bugs.md` issue "Isolated delegation worktree snapshots omit untracked files" is fixed by mirroring or explicitly rejecting untracked dirty state.

If either spawn fails, report the failure clearly and stop unless one reviewer was already created; in that case continue to Step 3 for the created reviewer and mark the missing reviewer as failed.

## Step 3: Wait for both reviewers

Use TermAl's delegation wait/fan-in surface to wait for both delegated reviewers to complete.

Prefer one of these paths, in order:

1. If a synchronous TermAl fan-in wait tool is available and returns reviewer results into this same turn, use it and then continue to Step 4.
2. If only the parent-scoped backend resume wait is available, schedule that wait, report the wait id and reviewer child session ids, then stop this turn immediately. Do not continue to Step 4 until TermAl resumes the parent with the fan-in prompt.

Never combine a backend resume wait with a manual polling loop in the same parent turn. A backend resume wait queues its result as the next parent prompt; keeping the parent turn active with PowerShell, shell, HTTP polling, or session-log polling prevents that queued fan-in prompt from running and can make the review appear stuck.

## Step 4: Consolidate results

After both reviewers finish, fetch each delegation result packet and present a concise fan-in:

```markdown
# Delegated Review

## Codex /review-local
- Status: ...
- Findings: ...
- Changed files: ...
- Commands run: ...

## Claude /review-local
- Status: ...
- Findings: ...
- Changed files: ...
- Commands run: ...

## Consolidated Action
- Critical/High: ...
- Medium/Low: ...
- Notes: ...
```

Deduplicate findings. If both reviewers report the same issue, merge it and note that both caught it.

## Step 5: Update `docs/bugs.md`

If either delegated reviewer reports any issue, note, test gap, stale bug-ledger item, or follow-up, update `docs/bugs.md` before completing:

- Add new active bug entries for untracked actionable findings.
- Update existing entries when a finding is already tracked.
- Remove active bug entries or implementation tasks that the reviewed changes fixed.
- Add implementation-task items for test gaps.

If both reviewers report no findings and no bug-ledger cleanup is needed, tell the user `bugs.md is up to date - no changes needed.`

Do not modify any files other than `docs/bugs.md`.
