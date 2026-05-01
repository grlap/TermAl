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
- Make child work visible and auditable in the UI.
- Let agents and humans query status, wait for completion, cancel work, and
  retrieve a compact result.
- Keep child sessions as normal TermAl sessions so existing transcript,
  persistence, SSE, stop, and approval behavior still apply.
- Support read-only reviewer/explorer tasks first.
- Support worker tasks only with explicit ownership and isolation rules.
- Let the parent resume from a structured result instead of reading the entire
  child transcript.

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
structured result. The parent can continue local work while children run, then
poll or wait for results.

Delegations are not special agent runtimes. They are metadata and control
surfaces around ordinary sessions.

## Terminology

- **Parent session**: the session that asked for delegation.
- **Child session**: the spawned ordinary TermAl session.
- **Delegation**: persisted link and lifecycle metadata between parent and child.
- **Reviewer delegation**: read-only child task that reports findings.
- **Worker delegation**: child task allowed to modify files inside an explicit
  ownership scope.
- **Result packet**: compact, structured output returned to the parent.

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

The child remains independently openable. TermAl should not hide its transcript.

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
      "status": "passed"
    }
  ],
  "notes": []
}
```

The packet is a summary for resumption, not a replacement for the child
transcript.

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

### 4. Resume

The parent can consume the result in one of three ways:
- human opens the card and reads it
- human inserts the result packet into the composer
- agent calls `get_delegation_result` and chooses how to continue

Automatic parent prompting should be opt-in.

### 5. Cancel

Cancel stops the child session and marks the delegation canceled. The parent
card should preserve partial transcript access and any partial result summary.

## Command And Tool Surface

### Internal Commands

TermAl should expose internal commands that can be used from the UI and from an
MCP wrapper:

```text
spawn_delegation(parentSessionId, request) -> DelegationRecord
get_delegation_status(delegationId) -> DelegationStatus
get_delegation_result(delegationId) -> DelegationResult
cancel_delegation(delegationId) -> DelegationStatus
```

### MCP Tools

Delegation tools are opt-in. TermAl should not expose agent-facing spawn/wait
tools until the user enables them for the current project or workspace. If
exposed through TermAl MCP, keep tool names explicit:

```text
termal_spawn_session
termal_get_session_status
termal_get_session_result
termal_cancel_session
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
- failed checks
- links or identifiers for diff/review artifacts

Safety limits for agent-facing tools:
- delegation ids are parent-scoped; a parent can only inspect, wait for, or
  cancel delegations it created unless the human grants broader scope
- default spawn permission is read-only
- per-parent concurrency and nesting-depth limits prevent unbounded process
  spawning
- every spawn, cancel, timeout, and result-read emits an auditable event
- child sessions cannot commit or push through TermAl-mediated commands unless
  the human explicitly approves that operation

### API Sketch

```http
POST /api/sessions/{parentSessionId}/delegations
GET  /api/delegations/{delegationId}
GET  /api/delegations/{delegationId}/result
POST /api/delegations/{delegationId}/cancel
```

There is no wait endpoint in Phase 1. A future internal command wrapper may
layer wait semantics over status polling or SSE recovery.

Delegation lifecycle changes should be revisioned delta events so normal SSE
gap detection and `/api/state` repair keep working:

```typescript
type DelegationDeltaEvent =
  | { type: "delegationCreated"; revision: number; delegation: DelegationSummary }
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

type DelegationRecord = {
  id: string;
  parentSessionId: string;
  childSessionId: string;
  mode: DelegationMode;
  status: DelegationStatus;
  title: string;
  prompt: string;
  cwd: string;
  agent: string;
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
```

Persist delegation records alongside sessions. A child session should also carry
`parentDelegationId` metadata so the relationship is recoverable after reload.

## Isolation Rules

Delegation write policies are runtime contracts, not just prompt text. V1 should
be explicit about which guarantees are enforced and which are advisory.

### Enforcement Model

`readOnly`:
- TermAl launches the child with the strictest available agent permission mode.
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
- Add result consolidation affordance.
- Keep consolidation human- or parent-agent-driven.

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
- future wait support returns on completion or timeout
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
- Should `wait_delegation` stream incremental status or return only final state?
- Should result extraction be purely prompt-convention based in v1, or should
  the backend ask the child agent for a structured final packet?
- How much of isolated worktree setup should TermAl own versus delegating to the
  child agent?
- Should parent cards be regular transcript messages or session metadata rendered
  inline?
