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

**IMPORTANT: This command must use TermAl MCP delegation tools to attempt exactly two reviewer session spawns. Do NOT use raw `claude -p`, Codex platform subagents, Claude Task agents, shell polling, raw HTTP, nested TermAl delegations, or any non-TermAl MCP review path to spawn or wait for reviewers. The delegated child sessions execute `/review-local` in read-only TermAl reviewer mode, where nested reviewer spawning is explicitly disabled. If the required TermAl MCP tools are unavailable, stop and report that `/review-with-delegate` requires the TermAl delegation MCP bridge.**

Delegated child reviewers run with `writePolicy: readOnly`. They may use read-only git/file inspection commands freely, but must not edit files, run mutating git commands, launch nested reviewer agents, run build/typecheck gates, or update the beads tracker themselves. The parent session owns the compile/typecheck precheck in Step 2 and applies any beads updates in Step 6.

Required MCP tools:
- `termal_spawn_session`
- `termal_get_session_status`
- `termal_get_session_result`
- `termal_resume_after_delegations`

## Step 1: Confirm review target

Run `git status --short`, `git diff --name-only`, `git diff --cached --name-only`, and `git ls-files --others --exclude-standard`.

If there are no staged, unstaged, or untracked changes, tell the user there is nothing to review and stop.

## Step 2: Precheck

Run `cargo check` in the parent session before spawning reviewers.
If it produces ANY errors, stop immediately and present the output.
Warnings are acceptable; report them and continue.

Then run `cd ui && npx tsc --noEmit`.
If it produces ANY errors, stop immediately and present the output.

These checks intentionally run in the parent session rather than read-only delegated children because Cargo and TypeScript may need to write build artifacts, caches, or lock files such as `target/debug/.cargo-lock`.

## Step 3: Spawn delegated reviewers

Using `termal_spawn_session`, create two child delegation sessions from the current parent session:

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

Use read-only delegation sessions here so reviewers see the exact current worktree, including untracked files. Do not request `isolatedWorktree` for this command until the known "isolated delegation worktree snapshots omit untracked files" limitation is fixed by mirroring or explicitly rejecting untracked dirty state.

If either spawn fails, report the failure clearly and stop unless one reviewer was already created; in that case continue to Step 4 for the created reviewer and mark the missing reviewer as failed.

## Step 4: Wait for both reviewers

Use TermAl MCP wait/fan-in tools to wait for both delegated reviewers to complete.

Call `termal_resume_after_delegations` with both delegation ids and `mode: "all"`, report the wait id and reviewer child session ids, then stop this turn immediately. Do not continue to Step 5 until TermAl resumes the parent with the fan-in prompt.

Never use `termal_wait_delegations`, PowerShell, shell, raw HTTP polling, or session-log polling for `/review-with-delegate` review fan-in. `termal_wait_delegations` is reserved for short smoke tests and diagnostics outside this command. A backend resume wait queues its result as the next parent prompt; keeping the parent turn active prevents that queued fan-in prompt from running and can make the review appear stuck.

## Step 5: Consolidate results

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

## Step 6: Update beads (bd)

If either delegated reviewer reports any issue, note, test gap, stale tracker item, or follow-up, update the beads tracker before completing (the parent owns writes; reviewers query read-only):

- `bd create -t bug -p <0-4> -d "..."` for untracked actionable findings (`-t task` for test gaps and follow-ups).
- `bd update <id>` (or `bd comment <id>`) when a finding is already tracked.
- `bd close <id>` for any issue the reviewed changes fixed.

If both reviewers report no findings and no tracker cleanup is needed, tell the user `beads is up to date - no changes needed.`

Do not modify source or test files; the only tracker updates are through `bd`.
