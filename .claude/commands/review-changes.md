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

Delegated `/review-code` children submit their authoritative result through the
versioned `termal_submit_review_result` mailbox contract. The backend validates
that payload and projects it into `termal_get_session_result`; reviewer prose is
retained only as paged full output. Never infer a clean review from prose. If a
required structured submission is missing, TermAl reports the delegation result
as failed/unavailable rather than returning an empty findings list.
TermAl injects this result protocol into every reviewer-mode child; the
repository's `/review-code` command does not need to contain the submission
schema or an opt-in marker.

Required MCP tools:
- `termal_spawn_session`
- `termal_get_session_status`
- `termal_get_session_result`
- `termal_resume_after_delegations`

## Step 1: Confirm review target

Run `git status --short`, `git diff --name-only`, `git diff --cached --name-only`, and `git ls-files --others --exclude-standard`.

If there are no staged, unstaged, or untracked changes, tell the user there is nothing to review and stop.

## Step 2: Parent quality gates

The parent owns gate execution. Use the maintained CLI once from the repository root:

```bash
node scripts/test-launcher.mjs full
```

The script owns the stage order, working directories, native tool resolution,
fingerprint, fail-fast execution, logs and diagnostic summary. Do not run the
individual gates manually. Do not import launcher functions from inline Node,
override its stage array, reconstruct the sequence, or write another runner.

Launch with a supported completion wake and end the turn immediately after
launch is acknowledged. Resume only on that completion notification. Do not
keep the turn alive with repeated execution waits, poll processes, tail logs,
count passing tests, or narrate unchanged status.

For an existing authorized root worker notifying a different coordinator, the
supported CLI is:

```bash
node scripts/test-launcher.mjs full --detach --notify COORDINATOR_SESSION_ID
```

Preserve the real sender identity; never spoof TERMAL_SESSION_ID or self-send.
Reviewer children must never run gates. The parent-session route requires a
host completion wake: if none is available, report that launcher integration
gap and stop. Do not silently substitute foreground wait loops or delegate
the review command itself.

Keep the run directory from the launch receipt. On completion, read only:
`node scripts/test-launcher.mjs summary RUN_DIRECTORY`.
Report failures and warnings with log paths; inspect bounded log excerpts only
to diagnose them. Missing terminal evidence is UNKNOWN, not PASS. Notification
recovery uses `node scripts/test-launcher.mjs notify RUN_DIRECTORY`, never a
new test run. A run whose launcher died without a terminal result is settled
with `node scripts/test-launcher.mjs recover RUN_DIRECTORY`: it becomes an
interrupted failure, never a pass, and only the run's owner sends its
completion. Reuse completed evidence for unchanged input; do not edit tested
input while running. Only a successful full run for the current input permits
Step 3. See docs/test.md for the launcher contract, not an alternate gate plan.

If any quality gate in this step produces a failure or error, stop the remaining
gate sequence and do not spawn reviewers, but do not stop at merely presenting
the raw output. Immediately investigate the failing path in this parent session
with falsifiable hypotheses and focused discriminating diagnostics, then classify
the root cause as a product defect, test/runner defect, or environment/resource
issue. Preserve the original run and logs; do not blindly rerun an unchanged
gate, and do not treat a later pass as diagnosis or closure. Fix confirmed
in-scope test or runner defects without another user approval round trip. Never
automate retry/repair/reviewer loops, weaken or ignore tests, inflate timeouts,
or change product semantics to obtain green. Escalate when the evidence requires
product behavior changes, destructive or external actions, missing authority,
or a genuine blocker. An intermittent symptom is never resolved by labeling it
"flaky." Search Beads for existing work and create or update the matching item
with the failure evidence and next action.
This pre-review gate-failure record is an explicit exception to the Step 5
tracker-timing rule; it must cover only the failed gate, not unreviewed code
findings. Present the relevant original failure excerpt and full-log path together
with the diagnosis and tracker action, not the entire run's output. Do not resume
the gate sequence or spawn reviewers until the original
required gate succeeds.

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

Treat each fetched compact packet as authoritative because it is backed by the
validated mailbox submission. If a reviewer status is failed or its result says
the structured submission is unavailable, report that reviewer as unavailable
and do not replace it with conclusions inferred from the full Markdown output.
Paged full output may be shown for diagnosis, but it is not a result protocol.

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

Outside the explicitly authorized Step 2 gate-failure remediation, this is an
ordinary review workflow: do not modify source or test files, and make tracker
updates only through `bd`. Delegated reviewer children remain inspection-only;
the Step 2 exception never authorizes product-semantic changes.
