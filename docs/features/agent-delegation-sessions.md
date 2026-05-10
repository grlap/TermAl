# Feature Brief: Agent Delegation Sessions

## Status

Proposed.

This brief defines an ad hoc delegation model for spawning bounded child agent
sessions from a parent conversation. It is intentionally smaller than the
existing orchestration-template system: delegation sessions are for immediate
parallel work, review lenses, focused investigations, and small isolated
patches that a lead agent can later resume from.

Related:
- [Orchestration](./orchestration.md)
- [Concurrent Session Link Cards](./concurrent-session-link-cards.md)
- [Diff Review Workflow](./diff-review-workflow.md)

## Problem

TermAl already supports many ordinary agent sessions, but a lead agent cannot
directly create a bounded side session, wait for it, and consume a compact
result without relying on the human to manually create tabs, copy prompts, and
paste summaries back.

That makes parallel work awkward in exactly the places where it is safest:
review lenses, read-only codebase exploration, targeted test additions, and
small patches with explicit file ownership.

## Goals

- Let a parent session spawn one or more child sessions for bounded subtasks.
- Make child work visible and auditable from the parent delegation UI.
- Let agents and humans query status, schedule backend-owned waits, cancel work,
  and retrieve a compact result.
- Keep child sessions as normal TermAl sessions so existing transcript,
  persistence, SSE, stop, and approval behavior still apply.
- Support read-only reviewer/explorer tasks first.
- Support worker tasks only with explicit ownership and isolation rules.
- Let the parent yield while children run, then resume from a structured fan-in
  result instead of polling or reading the entire child transcript.

## Non-goals for v1

- No hidden autonomous swarm behavior.
- No automatic commits or pushes.
- No automatic merge from child worktrees into the main workspace.
- No dependency scheduler or reusable workflow graph in v1; use orchestration
  templates for graph workflows.
- No cross-machine delegation until project-scoped remotes can represent the
  safety model clearly.

## Core Idea

A delegation is a parent-child relationship between two normal sessions:

```text
Parent session
  |
  +-- delegation-1 -> child session: "React review"
  +-- delegation-2 -> child session: "Rust review"
  +-- delegation-3 -> child session: "Implement tests"
```

The child session receives a bounded prompt, runs independently, then records a
structured result. The parent can either continue local work or explicitly yield
on a backend-owned wait. When the wait condition is met, TermAl queues a resume
prompt and wakes the parent through the normal queued-prompt dispatcher.

Delegations are not special agent runtimes. They are metadata and control
surfaces around ordinary sessions.

## Value To Parent Agents

Delegation is useful to a parent agent even when that agent already has an
internal subagent mechanism. Internal subagents are scoped to one runtime and
usually disappear into that runtime's transcript. TermAl delegations make the
parallel work a durable application feature:

- **Cross-agent fan-out**: a parent can ask Claude, Codex, Cursor, and Gemini to
  inspect the same work independently, instead of being limited to one
  provider's internal helper model.
- **Durable child sessions**: delegated work is persisted as normal TermAl
  sessions with openable transcripts, lifecycle state, cancellation, and result
  packets.
- **Backend fan-in**: a parent can yield on `all` or `any` delegation waits and
  let TermAl resume it with a consolidated result prompt instead of polling or
  manually copying summaries.
- **Enforced isolation**: read-only mode, isolated worktrees, ownership scopes,
  parent settings, and project context are enforced by TermAl rather than only
  by prompt discipline.
- **Human visibility and control**: delegated work remains visible from the
  parent delegation card so the user can inspect child sessions, follow
  progress, cancel work, or act on a result before the parent continues.

## Terminology

- **Parent session**: the session that asked for delegation.
- **Child session**: the spawned ordinary TermAl session.
- **Delegation**: persisted link and lifecycle metadata between parent and child.
- **Reviewer delegation**: read-only child task that reports findings.
- **Worker delegation**: child task allowed to modify files inside an explicit
  ownership scope.
- **Result packet**: compact, structured output returned to the parent.
- **Delegation wait**: durable parent-owned fan-in record that watches one or
  more delegations and resumes the parent when `any` or `all` are terminal.
- **Yielding parent**: a parent session with at least one pending delegation wait
  and no active turn. The parent is idle from the agent runtime's perspective,
  but TermAl shows that it is waiting for delegated work.

## Product Model

### Delegation Card

The parent transcript should show a delegation card when child sessions are
spawned.

The card contains:
- child name
- agent/provider/model
- status
- elapsed time
- working directory
- write mode
- files owned, if any
- latest status summary
- open child session button
- cancel button while running
- insert result into prompt button when complete

For multiple parallel children, the parent can show a grouped card:

```text
Delegated Work
3 sessions running

React Review       running    02:14
Rust Review        complete   01:32    1 finding
Security Review    complete   01:45    clean
```

### Child Session

The child is a normal session with:
- a generated title
- parent delegation metadata
- original task prompt
- optional file ownership constraints
- optional result-format instructions

The child remains independently openable. TermAl should not prune its
transcript.

### Retention And Cleanup

Delegation children are durable ordinary session records for auditability and
restart recovery, but default session lists omit sessions with
`parentDelegationId`. Parent delegation cards and result links are the primary
reopen affordance for child transcripts, so reviewer fan-out does not clutter the
sidebar while preserving access to the full child session.

Open item: define a backend archive/prune policy for long-lived installations.
Any storage-level cleanup must preserve delegation result packets, parent cards,
transcript links, and an explicit way to reopen or export the child session on
demand.

### Result Packet

When a child finishes, TermAl records a compact result packet:

```json
{
  "delegationId": "delegation-123",
  "childSessionId": "session-456",
  "status": "completed",
  "summary": "Reviewed the virtualized transcript changes. One test gap remains.",
  "findings": [
    {
      "severity": "Low",
      "file": "ui/src/panels/VirtualizedConversationMessageList.test.tsx",
      "line": 612,
      "message": "Programmatic release path lacks a post-idle unmount assertion."
    }
  ],
  "changedFiles": [],
  "commandsRun": [
    {
      "command": "npx vitest run src/panels/VirtualizedConversationMessageList.test.tsx",
      "status": "success"
    }
  ],
  "notes": []
}
```

The packet is a summary for resumption, not a replacement for the child
transcript. Command status labels use the backend vocabulary: `running`,
`success`, or `error`.

## Lifecycle

### 1. Spawn

The parent requests a child with:
- prompt
- working directory
- agent/provider
- optional model
- mode: `reviewer`, `explorer`, or `worker`
- write policy
- optional file ownership list
- optional timeout
- optional result schema

TermAl creates:
- child session
- delegation record
- parent delegation card
- SSE update for both parent and child surfaces

Phase 1 REST spawn does not park records in `queued`: it either creates a
`running` delegation immediately or rejects with `409` when the per-parent
active limit is full. `queued` is reserved for a future scheduler/throttle layer
that would own the queued-to-running transition and emit `delegationUpdated`
when dispatch actually starts.

### 2. Run

The child runs like any other session.

The parent is not blocked unless it explicitly waits. The UI should keep the
child status visible without forcing the parent transcript to hydrate the child
transcript.

### 3. Complete

Completion happens when the child session is idle and has produced a final
assistant response. TermAl extracts or requests a result packet from the final
response.

For v1, the result can be derived from the child final response using a clear
prompt contract. Later, TermAl can add a native structured result message type.

### 4. Resume / Yield

The parent can consume the result in one of three ways:
- human opens the card and reads it
- human inserts the result packet into the composer
- agent calls `get_delegation_result` and chooses how to continue

Automatic parent prompting is opt-in through a delegation wait. A wait records a
parent session, one or more delegation ids, and a fan-in mode:

- `any`: resume the parent when the first watched delegation reaches a terminal
  state.
- `all`: resume the parent only after every watched delegation reaches a
  terminal state.

When the wait is scheduled, the parent can yield the current turn instead of
polling. TermAl persists the wait and exposes it through `/api/state` and SSE so
the UI can show "waiting for delegations" even after reload.

When the wait is satisfied, TermAl queues a synthesized prompt to the parent
session and removes the wait from the pending-wait list. If the parent is idle,
the prompt dispatches immediately. If the parent is still in a turn, the prompt
waits behind the current turn and resumes the parent through the existing
queued-prompt path.

If the parent session is removed or becomes unavailable before a wait can
resume it, TermAl consumes the parent's pending waits with
`reason: "parentSessionRemoved"` or `reason: "parentSessionUnavailable"` and
does not queue a resume prompt. This keeps `/api/state` from retaining orphan
waits and lets SSE clients distinguish normal fan-in completion from parent
loss.
Boot-time reconciliation applies the same cleanup to persisted waits whose
parent session is already missing.

The resume prompt is deliberately close to orchestration's consolidated
transition prompt: it includes the wait id, mode, watched delegation statuses,
and one result section per terminal child. `all` waits produce a full fan-in
bundle; `any` waits produce the first terminal result plus the current status of
the remaining children.

Delegation waits reuse the orchestration scheduling model conceptually: child
delegations are completion sources and the parent is the destination session.
`all` corresponds to orchestration's `Consolidate` input mode; `any`
corresponds to ordinary queued transition delivery. The delegation API keeps
this ad hoc so users do not need to author a reusable orchestration template for
one-off reviewer batches.

Example flow:

```text
spawn_delegation(agent="Claude", prompt="Review backend resolver") -> delegation-a
spawn_delegation(agent="Codex", prompt="Review frontend composer") -> delegation-b
resume_after_delegations(parentSessionId, [delegation-a, delegation-b], mode="all")

...parent yields; TermAl shows a pending all-mode delegation wait...

...both children finish...

TermAl queues a parent resume prompt containing both results and starts the
parent if it is idle.
```

For reviewer fan-out, callers can combine spawn and fan-in scheduling:

```text
spawn_reviewer_batch(parentSessionId, requests, { mode: "all", title: "Review fan-in" })

...TermAl creates child sessions, stores one delegation wait for successful spawns...

...the parent yields...

TermAl queues the parent resume prompt when all successful children finish.
```

The reviewer-batch path is the preferred API for "spawn several reviewers and
wait for all of them." It creates all successful child sessions first, then
stores one `all` wait covering those delegation ids. Partial spawn batches still
schedule the wait for successful children; failed spawn items are returned in
the batch result so the parent can decide whether to retry.

### 5. Cancel

Cancel stops the child session and marks the delegation canceled. The parent
card should preserve partial transcript access and any partial result summary.

Cancel responses return the server's latest delegation status. The UI treats a
`failed` response as an error because the cancel was a no-op against an already
errored delegation. `completed` and `canceled` are idempotent terminal no-ops,
while `queued` and `running` can occur while the cancel request has been accepted
but follow-up state is still arriving through SSE.

### 6. Delegate Agent Commands

Delegating a slash command or future skill must not bypass command-template
resolution. The regular-send path and delegation path should both call the
backend command resolver described in
[`agent-slash-commands.md`](agent-slash-commands.md).

Required contract:

- The frontend passes `command`, `arguments`, optional `note`, and
  `intent: "delegate"` to the backend resolver.
- The backend loads the command/skill template, replaces `$ARGUMENTS` with the
  `arguments` field, and appends a `## Additional User Note` block when `note`
  is present.
- The backend returns the resolved delegation prompt plus command-derived
  defaults such as `mode`, `title`, and `writePolicy`. The policy source is
  `metadata.termal` frontmatter, not hard-coded command names. The target trust
  boundary is TermAl-owned command or future `SKILL.md` metadata; the current
  implementation still accepts project-local `.claude/commands/*.md` metadata
  that passes the source/name gate, and tightening that is tracked in
  `docs/bugs.md`.
- `spawn_delegation` receives the already-resolved prompt and the resolver's
  write policy. React components must not special-case command names such as
  `review-local`.

This keeps `/fix-bug`, `/review-local`, and future Claude skills consistent
whether the user sends them in the parent session or delegates them to a child.

## Command And Tool Surface

### Internal Commands

TermAl should expose internal commands that can be used from the UI and from an
MCP wrapper:

Implementation: `ui/src/delegation-commands.ts`; wait-error packet
sanitization lives in `ui/src/delegation-error-packets.ts`.

```text
spawn_delegation(parentSessionId, request) -> SpawnDelegationCommandResult
spawn_reviewer_batch(parentSessionId, requests, resumeAfter?) -> SpawnReviewerBatchCommandResult
get_delegation_status(parentSessionId, delegationId) -> DelegationStatusCommandResult
get_delegation_result(parentSessionId, delegationId) -> DelegationResultPacket
cancel_delegation(parentSessionId, delegationId) -> DelegationStatusCommandResult
wait_delegations(parentSessionId, delegationIds, options?) -> WaitDelegationsResult
resume_after_delegations(parentSessionId, delegationIds, options?) -> DelegationWaitResponse
```

`spawn_reviewer_batch` is the first Phase 3 helper. It fans out several
read-only reviewer spawns in parallel through the same Phase 1 REST create route
and returns successful child ids plus per-item failures. `completed` means every
spawn succeeded on one backend instance; `partial` means at least one spawn
succeeded, at least one item failed, and every successful response came from the
same backend instance. `error` means every item failed or any successful
responses crossed backend instances during restart. Mixed-instance spawn errors
set `error.kind === "mixed-server-instance"`, null top-level revision metadata,
and include diagnostic `error.recoveryGroups`. The current command surface does
not accept a server-instance selector, so wrappers should treat mixed-instance
errors as non-recoverable through these helpers until a server-aware transport is
added.
`wait_delegations` returns `error.kind === "mixed-server-instance"` when a
successful or timed-out status batch observes a backend restart between polling
cycles or within one parallel status batch. Status-fetch failures have priority:
if any status request rejects, the result is `status-fetch-failed` even when
collected responses already include another `serverInstanceId`. Its
`error.recoveryGroups` are diagnostic only: groups identify which backend
instance produced each observed delegation/status pair, and a previous-instance
group is scoped to delegations fetched in the current poll. A single
`delegationId` can appear in multiple groups within one error packet: once for
the previous-instance baseline and once for the current instance response. If
the first poll crosses instances, the previous-instance group is omitted because
there is no baseline to report. Within each group, `delegationIds` and
`childSessionIds` are ordered by each delegation id's position in the original
`wait_delegations` request. Groups are ordered by the earliest requested
delegation id they contain, with `serverInstanceId` as the tie-breaker.
Revisions are per server instance and must not be compared across groups.

`spawn_reviewer_batch` can also take a third `resumeAfter` argument with the
same shape as `resume_after_delegations` options. When supplied, successful
spawns are followed by a backend resume wait for those delegation ids. Partial
spawn batches schedule the wait for only the successful child sessions and keep
the failed items in the batch result. Mixed-server-instance batches do not
schedule a wait because their successful ids came from different backend
instances.

`resume_after_delegations` does not poll in the caller. It schedules a durable
backend delegation wait for the parent session and returns the created wait
record. When the selected `any` or `all` condition is satisfied, the backend
queues a synthesized resume prompt to the parent through the normal
queued-prompt dispatcher. The default mode is `all`. Callers should treat a
successful scheduled wait as a yield point: do not poll in the same parent turn
unless the user explicitly asks for synchronous status. TermAl will re-activate
the parent when the wait completes.

Spawn commands return client-side validation failures as `outcome: "error"`
with `error.kind === "validation-failed"`. Wait commands are different:
invalid parent/delegation ids or wait options throw `TypeError`/`RangeError`
before polling or scheduling starts.

Spawn validation packet messages are intentionally allow-listed. Unknown spawn
validation exceptions collapse to `"Invalid delegation request."`; wrapper UX
should not depend on spawn packet messages outside this list:

- `parent session id must be a string`
- `parent session id must be non-empty`
- `parent session id must not contain /, ?, #, or control characters`
- `prompt must be a string`
- `prompt must be non-empty`
- `title must be omitted instead of null`
- `cwd must be omitted instead of null`
- `agent must be omitted instead of null`
- `model must be omitted instead of null`
- `mode must be omitted instead of null`
- `writePolicy must be omitted instead of null`
- `spawn_reviewer_batch requests must be an array`
- `spawn_reviewer_batch requires at least one reviewer`
- `prompt must be no larger than <MAX_DELEGATION_PROMPT_BYTES> bytes`
- `title must be no longer than <MAX_DELEGATION_TITLE_CHARS> characters`
- `model must be no longer than <MAX_DELEGATION_MODEL_CHARS> characters`
- `spawn_reviewer_batch accepts at most <MAX_REVIEWER_BATCH_SIZE> reviewers`
- `reviewer request N must be an object`

Wait validation throws `TypeError` or `RangeError` before polling starts. These
throws are not spawn validation packets and are not sanitized by
`delegation-error-packets.ts`. Wrappers should catch by error type for UX.
The current runtime message templates are pinned by delegation command tests for
wrapper diagnostics:

- `parent session id must be a string`
- `parent session id must be non-empty`
- `parent session id must not contain /, ?, #, or control characters`
- `delegation ids must be an array`
- `delegation id must be a string`
- `delegation id must be non-empty`
- `delegation id must not contain /, ?, #, or control characters`
- `wait_delegations requires at least one delegation id`
- `wait_delegations accepts at most <MAX_DELEGATION_WAIT_IDS> ids`
- `resume_after_delegations requires at least one delegation id`
- `resume_after_delegations accepts at most <MAX_DELEGATION_WAIT_IDS> ids`
- `delegation transport does not support backend-scheduled resume waits`
- `pollIntervalMs must be a finite positive duration`
- `timeoutMs must be a finite positive duration`
- `pollIntervalMs must be at least <MIN_DELEGATION_WAIT_INTERVAL_MS>ms`
- `timeoutMs must be no greater than <MAX_DELEGATION_WAIT_TIMEOUT_MS>ms`

Backend-scheduled resume wait failures may surface these sanitized backend
messages through `DelegationResumeWaitFailurePacket`:

- `delegation wait accepts at most <MAX_DELEGATION_WAIT_IDS> delegation ids`
- `delegation wait title must be at most <MAX_DELEGATION_TITLE_CHARS> characters`
- `delegation <id> does not belong to parent session <parentSessionId>`

Use the exported constants from `ui/src/delegation-commands.ts` as the numeric
source of truth. Angle-bracket placeholders above interpolate these values in
runtime strings:

- `MAX_DELEGATION_PROMPT_BYTES = 65536`
- `MAX_DELEGATION_TITLE_CHARS = 200`
- `MAX_DELEGATION_MODEL_CHARS = 200`
- `MAX_REVIEWER_BATCH_SIZE = 4`
- `MAX_DELEGATION_WAIT_IDS = 10`
- `MIN_DELEGATION_WAIT_INTERVAL_MS = 500`
- `MAX_DELEGATION_WAIT_TIMEOUT_MS = 1800000`
- `DEFAULT_DELEGATION_WAIT_INTERVAL_MS = 1000`
- `DEFAULT_DELEGATION_WAIT_TIMEOUT_MS = 300000`
Grouped parent-card UI remains separate Phase 3 work. Backend-scheduled result
fan-in is available through `resume_after_delegations`.

### MCP Tools

Delegation tools are opt-in. TermAl should not expose agent-facing spawn/wait
tools until the user enables them for the current project or workspace. If
exposed through TermAl MCP, keep tool names explicit:

```text
termal_spawn_session
termal_get_session_status
termal_get_session_result
termal_cancel_session
termal_resume_after_delegations
```

Tool results should include enough information for a parent agent to continue
without opening the child transcript:
- delegation id
- child session id
- status
- summary
- findings
- changed files
- commands run
- links or identifiers for diff/review artifacts

Safety limits for agent-facing tools:
- delegation ids are parent-scoped; a parent can only inspect, wait for, or
  cancel delegations it created unless the human grants broader scope
- default spawn permission is read-only
- per-parent concurrency and nesting-depth limits prevent unbounded process
  spawning
- delegation titles are capped at 200 characters so redacted child-session
  names stay metadata-sized and are not a prompt-sized side channel
- explicit delegation model names are capped at 200 characters because they are
  persisted and echoed in summaries as metadata
- app-level default model preferences are also capped at 200 characters, and a
  blank value or `default` resets the preference to the selected agent's built-in
  default behavior
- omitted or blank delegation `model` values use the selected agent's app-level
  default model preference; explicit delegation models still override that
  preference for the child only
- composer-created delegations intentionally omit `model` unless the caller
  explicitly supplies one, so they follow the app-level default instead of a
  parent session's transient model override
- every spawn, cancel, timeout, and result-read emits an auditable event
- child sessions cannot commit or push through TermAl-mediated commands unless
  the human explicitly approves that operation

### API Sketch

```http
POST /api/sessions/{parentSessionId}/delegations
GET  /api/sessions/{parentSessionId}/delegations/{delegationId}
GET  /api/sessions/{parentSessionId}/delegations/{delegationId}/result
POST /api/sessions/{parentSessionId}/delegations/{delegationId}/cancel
POST /api/sessions/{parentSessionId}/delegation-waits
```

The `delegation-waits` endpoint schedules backend-owned parent resume prompts.
The polling `wait_delegations` helper remains client-side and does not mutate
parent session state.

`GET /api/state` includes pending `delegationWaits` so reloads and other tabs can
render the parent waiting state. Wait records are removed from the snapshot when
they are consumed. The synchronous `DelegationWaitResponse` still returns the
created wait even if it is instantly satisfied and consumed by a follow-up
revision. `resumePromptQueued` means TermAl queued a parent resume prompt.
`resumeDispatchRequested` is separate and only means that the parent was idle
enough for TermAl to dispatch that queued prompt immediately.

Delegation lifecycle changes should be revisioned delta events so normal SSE
gap detection and `/api/state` repair keep working:

```typescript
type DelegationDeltaEvent =
  | { type: "delegationCreated"; revision: number; delegation: DelegationSummary }
  | { type: "delegationWaitCreated"; revision: number; wait: DelegationWaitRecord }
  | {
      type: "delegationWaitConsumed";
      revision: number;
      waitId: string;
      parentSessionId: string;
      reason: "completed" | "parentSessionUnavailable" | "parentSessionRemoved";
    }
  | {
      type: "delegationUpdated";
      revision: number;
      delegationId: string;
      status: DelegationStatus;
      updatedAt: string;
    }
  | {
      type: "delegationCompleted";
      revision: number;
      delegationId: string;
      result: DelegationResultSummary;
      completedAt: string;
    }
  | {
      type: "delegationCanceled";
      revision: number;
      delegationId: string;
      canceledAt: string;
      reason?: string;
    };
```

`/api/state` must include enough delegation summary data to recover missed
lifecycle deltas after reconnect.

## Data Model

```typescript
type AgentType = "Claude" | "Codex" | "Cursor" | "Gemini";
type SessionStatus = "active" | "idle" | "approval" | "error";
type ApiRequestErrorKind = "backend-unavailable" | "request-failed";

type DelegationMode = "reviewer" | "explorer" | "worker";
type DelegationStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "canceled";

type DelegationWritePolicy =
  | { kind: "readOnly" }
  | { kind: "sharedWorktree"; ownedPaths: string[] }
  | { kind: "isolatedWorktree"; ownedPaths: string[]; worktreePath: string };

type DelegationWritePolicyRequest =
  | { kind: "readOnly" }
  | { kind: "sharedWorktree"; ownedPaths: string[] }
  | {
      kind: "isolatedWorktree";
      ownedPaths: string[];
      // Optional in requests/defaults; the backend generates a TermAl-owned
      // worktree path before persisting the delegation record.
      worktreePath?: string;
    };

type DelegationRecord = {
  id: string;
  parentSessionId: string;
  childSessionId: string;
  mode: DelegationMode;
  status: DelegationStatus;
  title: string;
  prompt: string;
  cwd: string;
  agent: AgentType;
  model?: string | null;
  writePolicy: DelegationWritePolicy;
  createdAt: string;
  startedAt?: string | null;
  completedAt?: string | null;
  result?: DelegationResult | null;
};

type DelegationResult = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  summary: string;
  findings?: DelegationFinding[];
  changedFiles?: string[];
  commandsRun?: DelegationCommandResult[];
  notes?: string[];
};

type DelegationFinding = {
  severity: string;
  file?: string | null;
  line?: number | null;
  message: string;
};

type DelegationCommandResult = {
  command: string;
  status: string;
};

type DelegationResultSummary = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  summary: string;
};

type DelegationSummary = Omit<DelegationRecord, "prompt" | "cwd" | "result"> & {
  result?: DelegationResultSummary | null;
};

type DelegationChildSessionSummary = {
  id: string;
  name: string;
  emoji: string;
  agent: AgentType;
  model: string;
  status: SessionStatus;
  parentDelegationId: string | null;
};

type SpawnDelegationFailurePacket =
  | {
      kind: "spawn-failed";
      name: string;
      message: string;
      apiErrorKind: ApiRequestErrorKind | null;
      status: number | null;
      restartRequired: boolean | null;
    }
  | {
      kind: "validation-failed";
      name: string;
      message: string;
    };

type SpawnDelegationCommandSuccessResult = {
  outcome: "completed";
  delegationId: string;
  childSessionId: string;
  delegation: DelegationSummary;
  childSession: DelegationChildSessionSummary;
  revision: number;
  serverInstanceId: string;
  error?: never;
};

type SpawnDelegationCommandResult =
  | SpawnDelegationCommandSuccessResult
  | {
      outcome: "error";
      revision: null;
      serverInstanceId: null;
      error: SpawnDelegationFailurePacket;
    };

type CreateDelegationRequest = {
  prompt: string;
  title?: string;
  cwd?: string;
  agent?: AgentType;
  model?: string;
  mode?: DelegationMode;
  writePolicy?: DelegationWritePolicyRequest;
};

type SpawnReviewerBatchItem = Omit<CreateDelegationRequest, "mode" | "writePolicy">;

type SpawnReviewerBatchFailure = {
  kind: "spawn-failed";
  index: number;
  title: string | null;
  name: string;
  message: string;
  apiErrorKind: ApiRequestErrorKind | null;
  status: number | null;
  restartRequired: boolean | null;
};

type DelegationWaitRecord = {
  id: string;
  parentSessionId: string;
  delegationIds: string[];
  mode: "any" | "all";
  createdAt: string;
  title?: string | null;
};

type StateResponse = {
  // Other fields omitted.
  delegations?: DelegationSummary[];
  delegationWaits?: DelegationWaitRecord[];
};

type DelegationWaitResponse = {
  revision: number;
  wait: DelegationWaitRecord;
  resumePromptQueued: boolean;
  resumeDispatchRequested: boolean;
  serverInstanceId: string;
};

type SpawnReviewerBatchResumeWaitResult =
  | {
      outcome: "scheduled";
      wait: DelegationWaitRecord;
      resumePromptQueued: boolean;
      resumeDispatchRequested: boolean;
      revision: number;
      serverInstanceId: string;
    }
  | {
      outcome: "skipped";
      reason: "mixed-server-instance" | "no-successful-spawns";
      message: string;
    }
  | {
      outcome: "error";
      error: {
        kind: "resume-wait-failed";
        name: string;
        message: string;
        apiErrorKind: ApiRequestErrorKind | null;
        status: number | null;
        restartRequired: boolean | null;
      };
    };

type SpawnReviewerBatchBaseResult = {
  spawned: SpawnDelegationCommandSuccessResult[];
  failed: SpawnReviewerBatchFailure[];
  delegationIds: string[];
  childSessionIds: string[];
  revision: number | null;
  serverInstanceId: string | null;
  resumeWait?: SpawnReviewerBatchResumeWaitResult;
};

type SpawnReviewerBatchCommandResult =
  | (SpawnReviewerBatchBaseResult & {
      outcome: "completed" | "partial";
      error?: never;
    })
  | (SpawnReviewerBatchBaseResult & {
      outcome: "error";
      error:
        | MixedServerInstanceErrorPacket
        | {
            kind: "all-spawns-failed";
            name: string;
            message: string;
          }
        | Extract<SpawnDelegationFailurePacket, { kind: "validation-failed" }>;
    });

type DelegationStatusCommandResult = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  delegation: DelegationSummary;
  revision: number;
  serverInstanceId: string;
};

type DelegationResultPacket = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  summary: string;
  findings: DelegationFinding[];
  changedFiles: string[];
  commandsRun: DelegationCommandResult[];
  notes: string[];
  revision: number;
  serverInstanceId: string;
};

type MixedServerInstanceErrorPacket = {
  kind: "mixed-server-instance";
  name: string;
  message: string;
  serverInstanceIds: string[];
  recoveryGroups: {
    serverInstanceId: string;
    revision: number;
    delegationIds: string[];
    childSessionIds: string[];
  }[];
};

type WaitDelegationErrorPacket =
  | {
      kind: "mismatched-delegation-id";
      name: string;
      message: string;
      requestedId: string;
      receivedId: string;
    }
  | MixedServerInstanceErrorPacket
  | {
      kind: "status-fetch-failed";
      name: string;
      message: string;
      apiErrorKind: ApiRequestErrorKind | null;
      status: number | null;
      restartRequired: boolean | null;
    };

type WaitDelegationsBaseResult = {
  delegations: DelegationSummary[];
  completed: DelegationSummary[];
  pending: DelegationSummary[];
  revision: number | null;
  serverInstanceId: string | null;
};

type WaitDelegationsSuccessResult = WaitDelegationsBaseResult & {
  outcome: "completed" | "timeout";
  error?: never;
};

type WaitDelegationsErrorResult = WaitDelegationsBaseResult & {
  outcome: "error";
  error: WaitDelegationErrorPacket;
};

type WaitDelegationsResult =
  | WaitDelegationsSuccessResult
  | WaitDelegationsErrorResult;
```

Persist delegation records alongside sessions. A child session should also carry
`parentDelegationId` metadata so the relationship is recoverable after reload.

## Isolation Rules

Delegation write policies are runtime contracts, not just prompt text. V1 should
be explicit about which guarantees are enforced and which are advisory.

### Enforcement Model

`readOnly`:
- TermAl launches the child with the strictest available agent permission mode.
- Claude read-only children are forced to Plan mode even when the user's
  default Claude mode is auto-approve; write-enabled Claude delegations keep
  the configured default mode.
- TermAl-mediated write/file/edit commands are disabled for the child.
- While the read-only delegation is running, TermAl also blocks local
  TermAl-mediated writes from parent or sibling sessions that target the same
  project/workdir scope. This fail-closed project lock prevents bypassing a
  reviewer delegation by switching session ids.
- The final result records commands run and declares whether any file changes
  were observed.
- If the underlying agent CLI cannot be hard-sandboxed, the UI must label the
  policy as "read-only requested" rather than "read-only guaranteed".

`sharedWorktree`:
- The parent must provide `ownedPaths`.
- TermAl canonicalizes owned paths before launch and rejects paths outside the
  project root.
- TermAl records changed files before result import and flags any file outside
  `ownedPaths`.
- Commit and push remain human-approved operations.

`isolatedWorktree`:
- TermAl creates or selects a TermAl-owned worktree root.
- The child runs inside that worktree, not the main workspace.
- `ownedPaths` still constrain the intended write set and review surface.
- Import back into the main workspace is an explicit compare/apply step.

### Path Validation

All path boundaries must be enforced server-side:
- normalize and canonicalize `ownedPaths`, `cwd`, and `worktreePath`
- reject `..`, absolute paths outside the project/worktree root, drive-relative
  Windows paths, UNC/device paths, and symlink/junction escapes
- keep isolated worktrees under a TermAl-owned root
- compare changed files by canonical path, not by string prefix
- treat path normalization failures as hard launch/import failures

### Reviewer And Explorer

Default to read-only. They may inspect files and run non-mutating commands.
They should not edit, stage, commit, or push. If the selected agent runtime
cannot enforce this, TermAl must say so in the child header and result packet.

### Worker

Worker delegation requires explicit ownership:
- owned paths or modules
- write policy
- expected output
- verification commands

Preferred worker mode is `isolatedWorktree`, especially if multiple workers
run in parallel. Shared-worktree workers are allowed only for small, explicitly
disjoint file sets.

### Commits And Pushes

Delegated sessions must not commit or push unless the human explicitly asks.
This mirrors the top-level TermAl safety policy.

## Prompt Contract

Spawner prompt should tell the child:
- it is a delegated child session
- whether it is read-only or writable
- which files it owns
- that other sessions may be active
- not to revert unrelated changes
- final answer must include structured result fields

Example reviewer final shape:

```markdown
## Result

Status: completed

Summary:
Reviewed the virtual-list patch. No blocking issues found.

Findings:
- None

Commands Run:
- npx vitest run src/panels/VirtualizedConversationMessageList.test.tsx: passed

Files Inspected:
- ui/src/panels/VirtualizedConversationMessageList.tsx
- ui/src/panels/VirtualizedConversationMessageList.test.tsx
```

## UI Placement

V1 can be minimal:
- composer "Delegate" action that spawns a read-only child from the current draft
- parent transcript delegation card
- child session opens as an ordinary tab
- session header shows parent link
- completed card has "Open child" and "Insert result" actions

Later:
- delegation drawer for all children of the active session
- grouped status for parallel reviewer batches
- result diff preview for worker children
- merge/import workflow for isolated worktree patches

## Relationship To Existing Orchestration

Delegation sessions are ad hoc. Orchestration templates are reusable graphs.

They should share primitives where possible:
- session creation
- transition/result summary logic
- parent/child link cards
- lifecycle status

Do not force ad hoc delegation through a template graph in v1. That would make
quick review/explorer tasks too heavy.

## Implementation Phases

### Phase 1: Read-only Delegation Records

- Add `DelegationRecord` persistence.
- Add parent-child link metadata.
- Add API to spawn a read-only child session with a prompt.
- Add status/result endpoints.
- Add SSE deltas for delegation lifecycle.
- Add parent transcript card.

### Phase 2: MCP/Internal Tool Surface

- Expose spawn/status/result/cancel commands.
- Return compact result packets.
- Add optional wait semantics, including waiting on multiple delegation ids.
- Add timeout behavior that does not cancel by default.

### Phase 3: Reviewer Batch UX

- Add helper command to spawn several read-only reviewers in parallel.
- Add grouped parent card.
- Add backend-scheduled result fan-in through parent resume waits, including a
  one-call reviewer-batch path.
- Keep UI result insertion human-driven unless the parent explicitly schedules a
  resume wait.

### Phase 4: Worker Delegation

- Add explicit owned-path validation.
- Add optional isolated git worktree creation.
- Track changed files and verification commands.
- Add import/compare path for worktree diffs.

### Phase 5: Integration With Orchestration

- Let orchestration templates create delegation-like child groups.
- Allow delegation result packets to feed transition prompts.
- Reuse cards and lifecycle events.

## Testing Plan

Backend:
- spawn creates child session and delegation record atomically
- persisted records reload with parent and child ids intact
- cancel stops child session and updates parent card
- result endpoint returns final packet after child completion
- SSE deltas are monotonic and recoverable through `/api/state`

Frontend:
- parent card appears after spawn
- card status updates through SSE
- child session opens from card
- result insertion fills composer without auto-sending
- canceled/failed/completed states render distinctly

MCP/internal commands:
- spawn returns delegation and child ids
- status works while running and after reload
- polling wait support returns on completion or timeout
- resume wait support queues a parent prompt after `any` or `all` child
  delegations finish
- result is unavailable until completion
- cancel is idempotent

Isolation:
- read-only delegation either disables writes or clearly labels the policy as
  advisory when the selected agent runtime cannot enforce it
- worker mode requires canonicalized owned paths
- isolated worktree mode never writes into the main worktree directly
- changed-file auditing flags any write outside the declared owned paths

## Acceptance Criteria

- A parent session can spawn a read-only child session from a bounded prompt.
- The parent transcript shows the child status.
- The child is a normal TermAl session and can be opened directly.
- The parent can retrieve a compact result packet after completion.
- The parent can continue work while the child runs.
- A canceled child leaves an auditable parent card and child transcript.
- Reload preserves delegation links and final results.
- No delegation path commits or pushes without explicit user action.

## Open Questions

- Should the first MCP surface be available to agents only, humans only, or both?
- Should `wait_delegations` stream incremental status or return only final state?
- Should result extraction be purely prompt-convention based in v1, or should
  the backend ask the child agent for a structured final packet?
- How much of isolated worktree setup should TermAl own versus delegating to the
  child agent?
- Should parent cards be regular transcript messages or session metadata rendered
  inline?
