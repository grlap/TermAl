---
name: review-changes
description: Review current changes by running /review-code in both Codex and Claude TermAl delegations.
metadata:
  termal:
    title:
      strategy: default
---

Review current staged and unstaged changes by delegating `/review-code` to both Codex and Claude through TermAl delegation sessions.

**IMPORTANT: Run `/review-changes` directly in the existing active, writable parent session. Never delegate or spawn `/review-changes` itself. The coordinator must be able to create normal build/test artifacts; only the `/review-code` children are delegated with `writePolicy: readOnly`.**

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. Read-only git commands (`diff`, `status`, `ls-files`, `show`, etc.) may be executed freely. Mutating git commands (`add`, `stash`, `checkout`, reset operations, etc.) may only be used when the current session write policy allows workspace mutation.**

**IMPORTANT: This command must use TermAl MCP delegation tools to attempt exactly two reviewer session spawns. Do NOT use raw `claude -p`, Codex platform subagents, Claude Task agents, shell polling, raw HTTP, nested TermAl delegations, or any non-TermAl MCP review path to spawn or wait for reviewers. The delegated child sessions execute `/review-code` in read-only TermAl reviewer mode, where nested reviewer spawning is explicitly disabled. If the required TermAl MCP tools are unavailable, stop and report that `/review-changes` requires the TermAl delegation MCP bridge.**

Delegated child reviewers run with `writePolicy: readOnly`. They may use read-only git/file inspection commands freely, but must not edit files, run mutating git commands, launch nested reviewer agents, run quality gates, inspect the existing Beads tracker, or call `bd`. Their `Suggested beads updates` sections are proposals only. The parent session exclusively owns all compilation, build, test, type-check, lint, and formatting gates; it first consolidates and deduplicates both reviews in Step 5, then reconciles the consolidated result with Beads in Step 6.

Required MCP tools:
- `termal_spawn_session`
- `termal_get_session_status`
- `termal_get_session_result`
- `termal_resume_after_delegations`

## Step 1: Confirm review target

Run `git status --short`, `git diff --name-only`, `git diff --cached --name-only`, and `git ls-files --others --exclude-standard`.

If there are no staged, unstaged, or untracked changes, tell the user there is nothing to review and stop.

## Step 2: Parent quality gates

This parent session is the only session in this workflow allowed to run quality gates. Run every command in this step before spawning reviewers; delegated `/review-code` children inspect code only and must not repeat these commands.

Run `cargo check` in the parent session before spawning reviewers.
If it produces ANY errors, stop immediately and present the output.
Warnings are acceptable; report them and continue.

Then run `cd ui && npx tsc --noEmit`.
If it produces ANY errors, stop immediately and present the output.

Then run `scripts/test-rust.sh` in the parent session. This wrapper raises the
Unix file-descriptor soft limit where possible and bounds Rust test
parallelism so FD-heavy fixtures do not make the gate flaky.
If it produces ANY failures or errors, stop immediately and present the output.

Then run `cd ui && npx vitest run` in the parent session.
If it produces ANY failures or errors, stop immediately and present the output.

These checks and tests intentionally run in the parent session rather than read-only delegated children because Cargo and frontend tooling may need to write build artifacts, caches, or lock files such as `target/debug/.cargo-lock`.

## Step 3: Spawn delegated reviewers

Using `termal_spawn_session`, create two child delegation sessions from the current parent session:

1. Codex reviewer
   - Agent: `Codex`
   - Prompt: `/review-code`
   - Mode: `reviewer`
   - Write policy: `readOnly`.
   - Title: `Codex /review-code`

2. Claude reviewer
   - Agent: `Claude`
   - Prompt: `/review-code`
   - Mode: `reviewer`
   - Write policy: `readOnly`.
   - Title: `Claude /review-code`

Use read-only delegation sessions here so reviewers see the exact current worktree, including untracked files. Do not request `isolatedWorktree` for this command until the known "isolated delegation worktree snapshots omit untracked files" limitation is fixed by mirroring or explicitly rejecting untracked dirty state.

If either spawn fails, report the failure clearly and stop unless one reviewer was already created; in that case continue to Step 4 for the created reviewer and mark the missing reviewer as failed.

## Step 4: Wait for both reviewers

Use TermAl MCP wait/fan-in tools to wait for both delegated reviewers to complete.

Call `termal_resume_after_delegations` with both delegation ids and `mode: "all"`, report the wait id and reviewer child session ids, then stop this turn immediately. Do not continue to Step 5 until TermAl resumes the parent with the fan-in prompt.

Never use `termal_wait_delegations`, PowerShell, shell, raw HTTP polling, or session-log polling for `/review-changes` review fan-in. `termal_wait_delegations` is reserved for short smoke tests and diagnostics outside this command. A backend resume wait queues its result as the next parent prompt; keeping the parent turn active prevents that queued fan-in prompt from running and can make the review appear stuck.

## Step 5: Consolidate results

After both reviewers finish, fetch each delegation result packet and present a concise fan-in:

```markdown
# Delegated Review

## Codex /review-code
- Status: ...
- Findings: ...
- Changed files: ...
- Commands run: ...

## Claude /review-code
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
Also merge their proposed tracker follow-ups into the consolidated action list.
Do not create, update, comment on, or close tracker items until this
consolidation is complete.

## Step 6: Reconcile consolidated findings with Beads (bd)

The writable parent owns this entire step. Reviewers neither inspect nor mutate
Beads. Use only the deduplicated findings and follow-ups produced in Step 5:

1. Search and inspect the existing tracker for each consolidated actionable
   finding or resolved issue. Suggested issue ids from reviewers are hints, not
   authoritative matches.
2. Deduplicate against existing work before making any tracker mutation.
3. Apply the appropriate parent-owned action:
   - `bd create -t bug -p <0-4> -d "..."` only for an actionable finding that
     is not already tracked (`-t task` for test gaps and follow-ups).
   - `bd update <id>` or `bd comment <id>` when the consolidated finding is
     already tracked.
   - `bd close <id>` only when the reviewed changes demonstrably fixed the
     tracked issue.
4. Do not create tracker work for purely informational observations that need
   no action.

If both reviewers report no findings and no tracker cleanup is needed, tell the user `beads is up to date - no changes needed.`

Do not modify source or test files; the only tracker updates are through `bd`.
