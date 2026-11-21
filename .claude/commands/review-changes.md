---
name: review-changes
description: Review a clean candidate commit against its base with two independent TermAl reviewers.
metadata:
  termal:
    title:
      strategy: default
---

Run this workflow from the writable parent session on a clean candidate commit.
Obtain two independent TermAl `/review-code` reviews, one Codex and one Claude.
Kimi may stand in for one unavailable reviewer.

**IMPORTANT: Run `/review-changes` directly in the existing active, writable parent session. Never delegate or spawn `/review-changes` itself. The coordinator must be able to create normal build/test artifacts; only the `/review-code` children are delegated with `writePolicy: readOnly`.**

**IMPORTANT: NEVER `git commit` or `git push` without explicit user approval. That is Greg's word or, for commit, push and install only, the standing rule in the Commit and push section of CLAUDE.md and AGENTS.md, or, for a candidate commit only, its candidate-commits bullet, or, for a test ref only, its test-ref bullets, or, for the data branch only, its task-sync data-branch bullet. Read-only git commands (`diff`, `status`, `ls-files`, `show`, etc.) may be executed freely. Mutating git commands (`add`, `stash`, `checkout`, reset operations, etc.) may only be used when the current session write policy allows workspace mutation.**

**IMPORTANT: This command must use TermAl MCP delegation tools to obtain the review pair — at most two reviews that count, one Codex and one Claude, with at most one Kimi stand-in replacing an unavailable Codex or Claude reviewer as Step 3 describes — so a round makes at most three spawns and counts no more than two reviews. A round that ends with fewer than two reviews reports the missing reviewer as unavailable, spawns no further reviewer, and does not satisfy the commit rule's review pair. Do NOT use raw `claude -p`, Codex platform subagents, Claude Task agents, shell polling, raw HTTP, nested TermAl delegations, or any non-TermAl MCP review path to spawn or wait for reviewers. The delegated child sessions execute `/review-code` in read-only TermAl reviewer mode, where nested reviewer spawning is explicitly disabled. If the required TermAl MCP tools are unavailable, stop and report that `/review-changes` requires the TermAl delegation MCP bridge.**

Delegated child reviewers run with `writePolicy: readOnly`. They may use read-only git/file inspection commands freely, but must not edit files, run mutating git commands, launch nested reviewer agents, run quality gates, mutate the tracker, or query tracker tasks to reconcile findings. Read-only startup recovery required by project instructions is allowed. Their `Suggested tracker updates` sections are proposals only. The parent session exclusively owns all compilation, build, test, type-check, lint, and formatting gates; it first consolidates and deduplicates both reviews in Step 5, then reconciles the consolidated result with the Engram tracker in Step 6.

Delegated `/review-code` children submit their authoritative result through the
versioned `termal_submit_review_result` mailbox contract. The backend validates
that payload and projects it into `termal_get_session_result`; reviewer prose is
retained only as paged full output. Never infer a clean review from prose. If a
required structured submission is missing, TermAl reports the delegation result
as failed/unavailable rather than returning an empty findings list.
TermAl injects this result protocol into every reviewer-mode child; the
repository's `/review-code` command does not need to contain the submission
schema or an opt-in marker.

When the reviewed change touches authority text, Step 7's final-diff audit
follows the review pair, and the change is neither landed nor pushed
before that audit is recorded on the item as passed.

Required MCP tools:
- `termal_spawn_session`
- `termal_get_session_status`
- `termal_get_session_result`
- `termal_resume_after_delegations`

## Step 1: Confirm review target

Confirm that the checkout is clean and inspect the candidate's change against
its recorded base. Require explicit `BASE` and `COMMIT` that resolve to commit
objects. Run these read-only checks from the candidate worktree:

```bash
git rev-parse HEAD
git status --porcelain --untracked-files=all
git diff --name-only BASE COMMIT --
```

HEAD must equal `COMMIT`, and status must be empty. All commands must succeed.
If the candidate has no changes, report that and stop.

### Freeze the review input

Independent reviews and landing gates run on clean candidate commits.
Staged inputs are for focused correction checks only. Reuse the launcher's
complete admitted `source_id` for the exact candidate when available.
Otherwise the parent obtains it with `engram source --root ABSOLUTE_WORKTREE --json`.
Record its locator, the root, the base and the changed paths on the item and
in the brief. Its commit must equal `COMMIT`. Keep the input unchanged
through validation and review.

Reviewers cite that record and read the diff from the base to its commit.
At the start and end, use read-only Git checks that HEAD equals that commit
and the checkout is clean. The parent checks the round against the
launcher's available observations for the same input as in Step 5.

Candidate commits and review adoption follow the candidate-commits bullet
in AGENTS.md and CLAUDE.md. A remote final gate follows their Test refs
bullet. Record the new candidate's complete `source_id` with the adoption.
A known intervening edit invalidates the affected result. Where correctness
depends on a property Git normalizes, check that property directly and
record what the check covers.

## Step 2: Parent quality gates

For a local landing gate, run the applicable gate before Step 3. When the
integration owner uses the Test refs route in AGENTS.md and CLAUDE.md, run
the focused compile and tests required by that bullet on the candidate
before Step 3. The remote landing gate runs on the pushed candidate after
the review pair has been verified and the test ref has been published.

Run the maintained launcher from the clean candidate checkout:

```bash
node scripts/test-launcher.mjs full --input commit
```

For a changeset touching only `.md` files, run the Markdown link check on
that same clean candidate commit. Both reviewers still review it.

```bash
node scripts/test-launcher.mjs focused --input commit -- node scripts/check-doc-links.mjs
```

Keep the candidate unchanged through the gate and review. A changed input
needs its own validation.

The script owns the stage order, working directories, native tool resolution,
input checks, fail-fast execution, logs and diagnostic summary. Do not run the
individual gates manually. Do not import launcher functions from inline Node,
override its stage array, reconstruct the sequence, or write another runner.

Launch with a supported completion wake and end the turn immediately after
launch is acknowledged. Resume only on that completion notification. Do not
keep the turn alive with repeated execution waits, poll processes, tail logs,
count passing tests, or narrate unchanged status.

For an existing authorized root worker notifying a different coordinator, the
supported CLI is:

```bash
node scripts/test-launcher.mjs full --input commit --detach --notify COORDINATOR_SESSION_ID
```

Preserve the real sender identity; never spoof TERMAL_SESSION_ID or self-send.
Reviewer children must never run gates. The parent-session route requires a
host completion wake: if none is available, report that launcher integration
gap and stop. Do not silently substitute foreground wait loops or delegate
the review command itself.

Keep the run directory from the launch receipt. On completion, read
`node scripts/test-launcher.mjs summary RUN_DIRECTORY` and the recorded
`source_id` values in `request.json` and `results.json`. Require the admitted
`request.sourceId` and both `results.sourceIdBefore` and
`results.sourceIdAfter` to equal the complete `source_id` recorded in Step 1.
A mismatch or unavailable identity is an evidence gap. These boundary
checks do not prove the absence of transient edits.
Report failures and warnings with log paths; inspect bounded log excerpts only
to diagnose them. Missing terminal evidence is UNKNOWN, not PASS. Notification
recovery uses `node scripts/test-launcher.mjs notify RUN_DIRECTORY`, never a
new test run. A run whose launcher died without a terminal result is settled
with `node scripts/test-launcher.mjs recover RUN_DIRECTORY`: it becomes an
interrupted failure, never a pass, and only the run's owner sends its
completion. Reuse completed evidence for unchanged input; do not edit tested
input while running. See docs/test.md for the launcher contract.

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
"flaky." Search Engram (`search`, `ls`) for existing work and add or note the
matching item with the failure evidence and next action.
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
   - Prompt: the multiline `/review-code` brief below
   - Mode: `reviewer`
   - Write policy: `readOnly`.
   - Title: `Codex /review-code`

2. Claude reviewer
   - Agent: `Claude`
   - Prompt: the multiline `/review-code` brief below
   - Mode: `reviewer`
   - Write policy: `readOnly`.
   - Title: `Claude /review-code`

Give each reviewer a multiline prompt whose first line tells it to read
`.claude/commands/review-code.md` and follow it. Include the working directory,
recorded base, candidate commit, changed paths and the parent's complete
`source_id` record locator to cite at both boundaries. The change to inspect
is `git diff BASE COMMIT --`, including added and deleted files. Do not pass
this context as arguments after a one-line `/review-code`: TermAl's one-line
command expansion has no arguments slot.

When Codex or Claude is unavailable (a usage limit or outage met in this round, with its refusal text recorded on the item), spawn Kimi in its place: Agent `Kimi`, Prompt: the same multiline `/review-code` brief as the reviewer it replaces, Mode `reviewer`, Write policy `readOnly`, Title `Kimi /review-code`. The stand-in replaces the unavailable reviewer, whether its spawn was refused or its result was lost to that reviewer's usage limit or outage, so a round never has more than two reviews that count. Never two reviewers of one vendor. Kimi never stands in for both reviewers of the same round: when Codex and Claude are both unavailable, only one stand-in is spawned, the other reviewer is reported as unavailable, and the round does not satisfy the commit rule's review pair. A round is one pass of Steps 3–5 over one unchanged candidate commit, as Step 5 requires. A Kimi reviewer's read-only gate is weaker than Claude's or Codex's (docs/features/kimi-cli-integration.md, Read-only delegation children); the stand-in is accepted only for an unavailable Codex or Claude.

Use read-only delegation sessions so reviewers see the worktree named in
the parent's record.

If either spawn fails, report the failure clearly. When the Codex or Claude spawn fails because that reviewer is unavailable (a usage limit or outage met in this round), record the refusal text on the item and, if no stand-in has been spawned this round, spawn Kimi in its place on the same freeze. For any other failure, or when the stand-in fails too, stop unless one reviewer was already created; in that case continue to Step 4 for the created reviewer and mark the missing reviewer as unavailable.

## Step 4: Wait for both reviewers

Use TermAl MCP wait/fan-in tools to wait for both delegated reviewers to complete.

Call `termal_resume_after_delegations` with the outstanding delegation ids (both reviewers at first; only the stand-in when it was spawned after a lost result) and `mode: "all"`, report the wait id and reviewer child session ids, then stop this turn immediately. Do not continue to Step 5 until TermAl resumes the parent with the fan-in prompt.

While the reviewers run, keep the reviewed files unchanged: reviewers read the live working tree, so an edit during the review makes it cover a moving target. Make independent edits in another worktree. A deliberate edit to the reviewed files needs a new gate and a new review; do not read the earlier result as covering it.

Keep `HEAD` at the recorded `COMMIT` and retain the clean checkout through
both reviews. A known intervening source or index
edit invalidates the affected result even if later boundary checks match.

Never use `termal_wait_delegations`, PowerShell, shell, raw HTTP polling, or session-log polling for `/review-changes` review fan-in. `termal_wait_delegations` is reserved for short smoke tests and diagnostics outside this command. A backend resume wait queues its result as the next parent prompt; keeping the parent turn active prevents that queued fan-in prompt from running and can make the review appear stuck.

## Step 5: Consolidate results

When the pair returns, verify both reviews. Resolve findings under Step 6.
A clean, verified review round satisfies the review condition for a remote
test-ref push under the Test refs bullet in AGENTS.md and CLAUDE.md.
For that path the applicable gate runs after the push. Landing requires the
applicable gate to pass on the same candidate commit with matching complete
admitted, before and after `source_id` values. For a Markdown-only candidate
the link check is the applicable gate.

Confirm each reviewer cited the parent's complete `source_id` and reported
matching read-only Git boundary checks for the supplied diff scope. Compare
the record with the launcher's available observations on the unchanged
input. Investigate missing or mismatched evidence. If the input changed,
renew the affected validation and review on the corrected input.

After both reviewers finish, fetch each delegation result packet and present a concise fan-in:

```markdown
# Delegated Review

## <vendor> /review-code
(one section per spawned reviewer: Codex, Claude, or the Kimi stand-in)
- Status: ...
- Findings: ...
- Changed files: ...
- Evidence: recorded command/error/unfinished/file counts; unavailable if no result.

## Consolidated Action
- Critical/High: ...
- Medium/Low: ...
- Notes: ...
```

The fan-in contains evidence counts. Fetch each result packet as required above
to inspect the full command and file lists, including which commands failed.
Summarize relevant failures in the report; zero recorded errors alone does not
establish a passing check.

Deduplicate findings. If both reviewers report the same issue, merge it and note that both caught it.
Also merge their proposed tracker follow-ups into the consolidated action list.
Do not create, update, comment on, or close tracker items until this
consolidation is complete. The Step 1 review-input record is an explicit
exception. The other exceptions are the
Step 2 gate-failure record and recording an unavailable Codex's or Claude's
refusal text (Steps 3 and 5): a
note on the Engram item the change lands under — the held item, or, when none
is held, the item the change belongs to — made when the refusal is met and
limited to that refusal.

Treat each fetched compact packet as authoritative because it is backed by the
validated mailbox submission. If a reviewer status is failed or its result says
the structured submission is unavailable, report that reviewer as unavailable;
when the reviewer is Codex or Claude and the cause is its unavailability met in this
round, record the refusal text on the item and, if no stand-in has been spawned this round,
spawn Kimi in its place on the same freeze, as Step 3 describes, then wait for
it through Step 4 and fetch its result before presenting; otherwise report the
reviewer as unavailable. When the stand-in's own submission is missing or failed, report
that reviewer as unavailable instead of spawning again. Never infer a clean
review from prose output, and do not replace a missing submission with
conclusions inferred from the full Markdown output. Paged full output may be
shown for diagnosis, but it is not a result protocol.

## Step 6: Reconcile consolidated findings with the Engram tracker

The writable parent owns this entire step. Reviewers may perform required read-only startup recovery, but do not reconcile findings with tracker tasks or mutate
the tracker. Use only the deduplicated findings and follow-ups produced in Step 5:

1. Search and inspect the existing tracker for each consolidated actionable
   finding or resolved issue. Suggested issue ids from reviewers are hints, not
   authoritative matches.
2. Deduplicate against existing work before making any tracker mutation.
3. Apply the appropriate parent-owned action:
   - `add` (kind `bug`, a priority 0-4 and an outcome) only for an actionable
     finding that is not already tracked (kind `task` for test gaps and
     follow-ups).
   - `note`, or `update` with `revise` for a field change, when the
     consolidated finding is already tracked.
   - `done` on a claim you hold, after its acceptance evaluation where it has
     criteria, only when the reviewed changes demonstrably fixed the tracked
     issue; for tracked work not done here (a false positive, a duplicate, or
     one already fixed elsewhere), `update` with `cancel` and a reason naming
     where it stands, or with `supersede` naming the replacement item.
4. Do not create tracker work for purely informational observations that need
   no action.

If both reviewers report no findings and no tracker cleanup is needed, tell the user `the tracker is up to date - no changes needed.`

Outside the explicitly authorized Step 2 gate-failure remediation, this is an
ordinary review workflow: do not modify source or test files, and make tracker
updates only through the `engram` tools. Delegated reviewer children remain
inspection-only; the Step 2 exception never authorizes product-semantic
changes.

## Step 7: Audit the final diff before a landing of authority text

Authority text is the text whose change conditions (a), (b) and (c) of the
Commit and push section of AGENTS.md and CLAUDE.md cover. When the reviewed
change touches it, the session that holds the final frozen input, the
parent or, where integration changed the input, the integration owner,
sends the final frozen change against its recorded base, the recorded
complete `source_id` and the review and launcher results, and the list of
recorded messages that word each changed passage of authority text to the
auditing coordinator by TermAl mailbox.
The change is neither landed nor pushed before the audit of that input
is recorded on the item as passed.
For an authority change using a remote test ref, complete the final-diff
audit before the test-ref push, using the exact focused compile and test
results required for that push. After the push, the remote landing gate
must pass on the same candidate commit and complete source_id before
master moves.

The auditing coordinator is the project's task coordinator; where the task
coordinator wrote the change, it is the other project's coordinator.
Before judging the authority passages, the auditing coordinator reads the
complete committed authority diff from the commit in the parent's
`source_id` record against its recorded base. Confirm it matches the supplied
diff, including each added authority file and its placement. With the
holder's confirmation that no check is carried in that root, verify the
read-only Git boundaries in Step 1 before and after reading the diff.
Record the base, root, source record locator and both boundary results with
the audit. Investigate a missing or mismatched result before passing the
audit. Boundary checks cannot exclude transient edits.

A changed passage is each sentence or heading of authority text that the
word-level diff of the change against its base commit adds, alters or
removes. Text that differs only in whitespace is not a changed passage.
The auditing coordinator reads the final frozen change line by line against the
sentences whose concurrence is recorded on the item, runs a script that
looks for each added or altered passage in those sentences, as an exact
string after whitespace normalisation and with Markdown list and quote
markers at line starts removed, and judges for each removed passage whether
a recorded concurrence names it as removed. The auditing coordinator notes
on the item the review `source_id`, which recorded message governs each
changed passage, the script's result for each added or altered passage and
the judgement for each removed one. The audit passes when the script finds
every added or altered passage and every removed passage is named as
removed, and fails otherwise. Other files in the frozen change are covered
by the gate and the review pair, not by the audit.

A passage of authority text that no recorded concurrence quotes whole is
concurred, word for word, or taken out before the landing. A change of the
input, such as taking a passage out, needs a new freeze, gate, review pair
and audit. A candidate committed afresh with an identical contribution adopts
the review pair under the Commit and push sections. Its own gate and audit
cite that candidate's recorded complete `source_id`. A concurrence recorded after the freeze leaves the input
unchanged: it needs no new freeze, gate or review pair, but the audit is
repeated, and the conditions of the Commit and push section hold for that
wording as for any other.
