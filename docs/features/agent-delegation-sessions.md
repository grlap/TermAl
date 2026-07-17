# Feature Brief: Agent Delegation Sessions

> Delegated Codex children run on the shared Codex app-server; its identity model
> and orphan-thread behavior are documented in
> [shared-codex-app-server.md](./shared-codex-app-server.md).

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
- [Code Navigation MCP](./code-navigation-mcp.md)

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

The same control surface generalizes past the parent-child tree. A delegation is
really a directed message to a session plus a reply and a backend fan-in; if the
target already exists instead of being spawned, the identical machinery becomes a
peer conversation between two top-level sessions. See
[Peer Session Connections](#peer-session-connections) below.

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
- **Connection**: a human-created, persisted edge between two existing top-level
  sessions that authorizes them to exchange messages. Unlike a delegation, it
  spawns nothing and owns neither session's lifecycle.
- **Exchange**: one directed prompt sent across a connection, with an optional
  single reply. The peer analogue of a delegation's task-plus-result.
- **Peer session**: either endpoint of a connection. Peers are symmetric; neither
  is a parent or child of the other.

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

The child remains independently openable from the parent delegation card while
the parent exists. TermAl omits delegated children from default session lists so
reviewer fan-out does not clutter the sidebar.

### Retention And Cleanup

Delegation children are durable ordinary session records while their parent
exists, which preserves restart recovery and lets users reopen child transcripts
from the parent card. The parent owns the child tree: deleting a parent session
cascades deletion to its delegated child sessions and any delegated descendants,
and tears down their runtimes.

This intentionally allows reviewer fan-out to accumulate child sessions during a
long parent session. The cleanup boundary is the parent session, not each
individual delegation result. That keeps child transcripts available for later
human inspection or follow-up prompts without making delegated reviewers visible
in default session lists.

Delegation tasks are one-shot records even though their child sessions remain
openable. A user may open a child transcript and continue it manually, but MCP
review automation should create a fresh child delegation for each bounded task
instead of reusing an earlier child session. Reuse-by-default would make result
packets ambiguous and would blur the parent-owned audit trail.

The delegation record and result summary remain in backend state for lifecycle
bookkeeping after parent deletion. Child transcripts and full result retrieval
through parent-scoped routes end with the owning parent. Open item: define a
later archive/export policy if long-lived installations need child transcripts
or full result packets after the owning parent is deleted.

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

This means a caller must choose between synchronous polling and backend resume
waits. After scheduling a backend resume wait, the parent turn should end; a
shell or HTTP polling loop in that same turn keeps the queued resume prompt
behind the active turn and can make the fan-in look stuck even though TermAl has
already queued the result.

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

UI messages that mention an unavailable child session derive their wording from
the same wire status: terminal states use "already ..." (`completed`, `failed`,
`canceled`) and in-flight states use "still ..." (`queued`, `running`). The
phrases are display text only; callers should branch on the wire status, not the
rendered message text.

In the current REST runtime, `running` delegations are expected to have a
`childSessionId`; a `running` response without one is treated as an unexpected
unavailable-child state. Childless `queued` records are reserved for the future
scheduler/throttle layer described above.

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
  defaults such as `mode`, `title`, and `writePolicy` when trusted command
  metadata is available. The policy source is trusted `metadata.termal`
  frontmatter, not hard-coded command names. Project-local
  `.claude/commands/*.md` frontmatter may influence resolver titles, but cannot
  grant delegation defaults.
- `spawn_delegation` receives the already-resolved prompt and the resolver's
  write policy. React components must not special-case command names such as
  `review-local`.
- The TermAl delegation MCP bridge applies the same rule for single-line
  prompts that match a known slash command: it resolves the command with
  `intent: "delegate"` before posting the delegation create request. When the
  tool call provides `cwd`, that same cwd is included in the resolve request so
  command discovery and trusted metadata come from the intended child workdir.
  If the command exists in the parent workdir but not the requested `cwd`, the
  spawn fails instead of sending an unexpanded slash command to the child.
  Literal prompts and truly unknown slash-like prompts remain unchanged.
- Caller-supplied MCP spawn options (`title`, `mode`, and `writePolicy`) override
  resolver-provided defaults. Omit those fields to use trusted command metadata.

This keeps `/fix-bug`, future trusted `/review-local` commands, and future
Claude skills consistent whether the user sends them in the parent session or
delegates them to a child.

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
collected responses already include another `serverInstanceId`. In that
priority path, collected responses from a different instance are ignored while
building the retained partial state, so the error can hide the concurrent
restart and omit mixed-instance `recoveryGroups`. Wrappers should treat
`status-fetch-failed` as "poll again or fall back to backend resume wait" rather
than as evidence that no restart happened. Mixed-instance
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
unless the user explicitly asks for synchronous status. If the caller needs
same-turn results, use `wait_delegations` instead of
`resume_after_delegations`. TermAl will re-activate the parent when the wait
completes after the current turn has yielded.

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

Current direction:
- Treat the parent session id as the v1 visibility boundary. Do not build a
  Linux-style namespace or capability-token system for the local bridge until a
  shared, remote, or cross-parent transport makes that necessary.
- Keep delegated child sessions durable and manually openable while the parent
  lives. They are one workflow tree, not disposable subprocesses; the parent
  deletion path is responsible for cascade cleanup.
- Prefer backend resume waits for review fan-in. A parent turn that schedules a
  resume wait must yield instead of shell-polling, raw-HTTP polling, or scraping
  session logs, otherwise the queued fan-in prompt cannot run.
- Expose the same TermAl-owned MCP bridge to Codex, Claude, Cursor, and Gemini
  startup/resume hooks. Agent commands such as `/review-with-delegate` should
  use only those tools and fail fast when the bridge is absent.

Delegation tools are parent-scoped. The first local implementation injects the
bridge into TermAl-launched agent runtimes by default, relying on the implicit
parent id, backend ownership checks, read-only default write policy, and
concurrency/depth limits as the safety boundary. Do not add a separate
namespace or capability-token layer for this local per-process bridge unless a
concrete agent integration requires it. Add project/workspace opt-in or
capability tokens before exposing the bridge over a shared, remote, or
long-lived reusable transport.

The parent session id is the namespace for v1. That is intentionally weaker
than a Linux-style namespace: child sessions are still ordinary TermAl sessions
in storage, and humans can open them from the parent card. The MCP caller,
however, only receives delegation ids created under its implicit parent and has
no tool that can enumerate unrelated sessions or delegations. This is the
minimum boundary needed for local delegated review automation without adding a
capability system prematurely.

Visibility is scoped at the tool boundary, not at the storage layer. The local
bridge is not expected to hide child sessions from TermAl itself, nor to make
children reusable by unrelated parent sessions. A delegated child may remain
openable from the parent UI for follow-up prompts while the parent lives. When
the parent session is deleted, TermAl owns cascade cleanup of its delegation
records and child sessions. That keeps review sessions useful during the parent
workflow while making long-term accumulation a parent-lifecycle concern instead
of a per-review cleanup requirement.

Do not implement a stronger namespace abstraction until there is a concrete
reason to do so. For the local per-process bridge, `parentSessionId` plus
backend ownership checks is the boundary. If a future transport is shared across
projects, exposed remotely, or reused across parent sessions, add an explicit
scope/capability layer at that point rather than weakening the v1 tool contract.

The first implementation is a TermAl-owned local MCP bridge spawned for one
parent agent session:

```text
termal delegation-mcp --parent-session-id <session-id> --base-url <http-origin>
```

The bridge is configured with the TermAl base URL and the current
`parentSessionId`; tool calls do not accept an arbitrary parent id. This keeps
the first security boundary simple: the bridge can only act under the parent
session that TermAl used to launch it, and the backend still validates that
every requested delegation belongs to that parent.

Do not add a broad "list all sessions" or "list all delegations" tool in the
first MCP slice. The bridge may return ids it created, and callers may pass
those ids back to status/result/cancel/wait tools. Broader visibility can be
added later behind an explicit human-granted scope if it proves useful. This is
the practical visibility boundary for v1: delegated children remain normal
sessions in storage, but parent-scoped MCP callers can only reach children by
delegation id through parent-owned routes.

**v2 update — peer messaging ratifies a broader boundary.** The delegation tools
above stay parent-scoped exactly as described. Peer messaging
(`termal_send_to_session`, `termal_list_sessions`) deliberately crosses this
boundary and ships the broad session listing the v1 slice deferred: a bridge MAY
enumerate and target **root** sessions across projects — and, on the roadmap,
across machines — because the point of peer messaging is long-running specialist
sessions on different projects consulting each other (for example, a Kadry coding
agent requesting changes from a LegalSystem coding agent). Delegation **children**
remain unreachable as peers: both tools filter to root sessions
(`parentDelegationId == null`). On top of that root-only filter,
`termal_send_to_session` refuses to target the caller *itself* — on both id and
name references — so a bridge cannot message itself; `termal_list_sessions`
applies only the root-only filter and so still lists the caller. That root-only
filter, plus send's self-rejection, is what the peer guard tests now pin, and it
is the actual v2 visibility boundary.

The exclusion is symmetric on the caller side: a bridge serving a delegation
child (a reviewer, explorer, or worker, which may be processing untrusted
content) is not given the peer tools at all. `tools_list_for_caller` removes
`termal_send_to_session` and `termal_list_sessions` from that child's advertised
tools, and the invocation path rejects them even if called directly, so a child
cannot reach root sessions *through the bridge*. That check fails closed: an
unreachable backend or an unresolvable caller is treated as a child and denied
the peer tools (tm-r0y). Only a root-session caller ever sees or invokes the peer
tools, so the note above that `termal_list_sessions` lists the caller is itself
scoped to a root caller.

This containment is a tool-layer guardrail, not process isolation. TermAl's
loopback HTTP API is unauthenticated under the single-user, local-only trust
model — `GET /api/state` and `POST /api/sessions/{id}/messages` answer any local
caller — so a child able to issue raw HTTP could enumerate or message sessions
directly, bypassing the bridge. Hiding and rejecting the peer tools keeps a
well-behaved agent within the boundary by governing the tools it is offered and
will run; a hard cross-session boundary would need caller-scoped REST auth, which
is deferred with the capability-token work (see Phase 3).

Keep tool names explicit:

```text
termal_spawn_session
termal_get_session_status
termal_get_session_result
termal_cancel_session
termal_wait_delegations
termal_resume_after_delegations
termal_followup_session
termal_send_to_session
termal_list_sessions
```

The MCP tools map to the existing command/API semantics:

```text
termal_spawn_session(request) -> SpawnDelegationCommandResult
termal_get_session_status({ delegationId }) -> DelegationStatusCommandResult
termal_get_session_result({ delegationId }) -> DelegationResultPacket
termal_cancel_session({ delegationId }) -> DelegationStatusCommandResult
termal_wait_delegations({ delegationIds, pollIntervalMs?, timeoutMs? }) -> WaitDelegationsResult
termal_resume_after_delegations({ delegationIds, mode?, title? }) -> DelegationWaitResponse
termal_followup_session({ delegationId, message }) -> DelegationStatusResponse
termal_send_to_session({ sessionId, message }) -> { sessionId, resolvedFrom, delivered }
termal_list_sessions() -> { sessions: [{ sessionId, name, agent, status, workdir, preview }] }
```

`termal_followup_session` re-arms a completed or failed delegation for another
turn — a still-running, canceled, or child-removed delegation is rejected (see
the `/followup` route). `termal_send_to_session` and `termal_list_sessions` are the
peer-messaging tools: `termal_list_sessions` returns root-session summaries for
discovery, and `termal_send_to_session` delivers a fire-and-forget message to a
root peer by id or name (queues if the peer is mid-turn). See the v2 visibility
boundary above and the shipped-vs-proposed note under *Peer Session Connections*.

`termal_wait_delegations` is a bounded synchronous wait for short waits and
smoke tests. `termal_resume_after_delegations` schedules the durable backend
wait and should be preferred for long-running delegated review flows because it
lets the parent yield and be resumed by TermAl when the wait is terminal.
Agents must not combine a backend resume wait with shell polling, raw HTTP
polling, or session-log scraping in the same parent turn. Once a resume wait is
scheduled, the parent should yield so the queued fan-in prompt can run as the
next turn.

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
- delegation ids are parent-scoped; the first MCP bridge is launched with one
  implicit parent session and can only inspect, wait for, cancel, or re-arm
  (follow up) delegations under that parent
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

Capability tokens are not required for the first local bridge as long as TermAl
spawns it per agent process, passes an implicit parent session id, and does not
expose it remotely. Treat capability tokens as remote/shared-transport work, not
as a prerequisite for local delegated review automation.

Non-goals for the local v1 bridge:
- hiding delegated child sessions from TermAl's own session storage
- making a delegated child reusable from another parent session
- adding project-wide delegation discovery tools
- adding capability-token issuance before there is a shared or remote transport
- treating read-only reviewer policy as a process sandbox when the selected
  agent runtime can only enforce it by instruction

Agent integration hooks:
- Codex sessions: pass a `config.mcp_servers.termal-delegation` descriptor in
  `thread/start` and `thread/resume`, using the same local executable and
  parent-scoped bridge arguments as the stdio bridge.
- ACP sessions: populate `mcpServers` in both `session/new` and `session/load`
  with the TermAl MCP bridge configuration.
- Cursor and Gemini sessions: use the same ACP `mcpServers` path as long as
  those ACP backends accept it; if a backend rejects inline `mcpServers`, fall
  back to a backend-specific generated local config from the same descriptor.
- Claude sessions: pass the bridge through Claude's `--mcp-config` process
  launch/resume path.
- All agents: if the bridge cannot be configured, commands that require
  delegated review must fail fast instead of silently falling back to raw HTTP,
  shell polling, Task agents, or Codex platform subagents.

`/review-with-delegate` depends on this MCP surface. Its final form should:
- verify `termal_spawn_session` and `termal_resume_after_delegations` are
  available before spawning reviewers
- spawn one Codex and one Claude read-only reviewer child session
- schedule a backend resume wait instead of shell polling
- fan in the returned result packets and update `docs/bugs.md` from the parent
  session only
- stop with a clear message when the TermAl delegation MCP tools are absent

Implementation order:
1. Close existing delegation correctness bugs first, especially terminal
   status/result refresh and backend resume wait behavior after restart.
2. Finish the local MCP bridge contract and regression coverage around
   parent-scoped spawn/status/result/cancel/wait tools.
3. Wire the same bridge descriptor into Codex, Claude, Cursor, and Gemini
   startup/resume hooks.
4. Rewrite `/review-with-delegate` to use only the TermAl MCP tools. The command
   must not fall back to raw HTTP, shell polling, Claude Task agents, Codex
   platform subagents, or manual session-log scraping.

### API Sketch

```http
POST /api/sessions/{parentSessionId}/delegations
GET  /api/sessions/{parentSessionId}/delegations/{delegationId}
GET  /api/sessions/{parentSessionId}/delegations/{delegationId}/result
POST /api/sessions/{parentSessionId}/delegations/{delegationId}/cancel
POST /api/sessions/{parentSessionId}/delegations/{delegationId}/followup
POST /api/sessions/{parentSessionId}/delegation-waits
```

The `delegation-waits` endpoint schedules backend-owned parent resume prompts.
The polling `wait_delegations` helper remains client-side and does not create
backend wait records. Its status/result reads may still refresh a completed
child delegation, persist the terminal delegation record, and consume any
already-satisfied backend wait watching that delegation.

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
      type: "delegationWaitResumeDispatchFailed";
      revision: number;
      parentSessionId: string;
      error: string;
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
// UI recovery category, not a strict HTTP status-class discriminator.
// Preserved parseable gateway JSON errors may report 502/503/504 as
// "request-failed"; branch on status/restartRequired when status-class
// behavior matters.
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
- TermAl keeps the child's configured agent permission mode. This lets reviewer
  delegations run normal inspection commands when the user's default allows it.
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

## Peer Session Connections

Proposed extension. Delegation always spawns its target and owns its lifecycle,
so the relationship is a tree. A common need does not fit that tree: two sessions
that already exist, started independently, that the user now wants to let talk to
each other — hand off context, ask a focused question, compare notes.

> **Status — what shipped diverges from this proposal.** The peer feature that
> actually ships is the simpler fire-and-forget messaging described under *MCP
> Tools* above: `termal_send_to_session` + `termal_list_sessions`. It has **no**
> connection object, **no** hop budget, **no** `expectsReply`, **no**
> `termal_reply_to_session`, and **no** human-only `/connect-sessions` — any root
> session may message any other by id or name, delivery is one-way
> fire-and-forget (a reply is just another incoming message), and agents initiate
> directly. In particular the *Provenance Is Mandatory* and hop-budget
> subsections below describe the **proposed** protocol and are **not
> implemented**: the shipped tool attaches the sender's identity as a
> backend-resolved transcript label for the human, but does **not** prepend a
> runtime provenance header, so the receiving agent sees an ordinary message and
> peers coordinate reachability in-band by convention. Observed autonomous
> consult/reply loops close without the header, so it is deferred as
> unproven-need. Treat everything below as future design, not current behaviour.

This is the same primitive with one thing removed. A delegation is a directed
message to a session plus a reply and a backend fan-in. Take away the spawn — let
the target be a session that already exists — and the delegation record becomes a
peer conversation. The underlying primitives are reused as-is — the status
lifecycle, the `all`/`any` wait, the queued-prompt resume, and SSE deltas. The
peer-facing surfaces intentionally differ: a reply is freeform prose rather than
the reviewer `## Result` packet, and both endpoints render a connection card
rather than one parent-owned card (see Data Model and Command And Tool Surface).

The single structural difference drives the whole design: neither peer is a
child, so no one owns the other's lifecycle, either side may initiate, and
termination cannot come from "the child finished." It comes from an explicit hop
budget instead.

### Turn-Taking Constraint

Sessions are turn-taking agents, not servers. A session can only consume input
between turns — the same fact that already forces a delegation parent to yield
its turn before a resume prompt can run. Peer messaging inherits this: there is
no synchronous "ask and block" call. Asking a peer means enqueue a prompt, end
your turn, and be resumed with the answer, exactly like
`resume_after_delegations`.

This makes the default interaction a **bounded ask/reply**:

- A sends an exchange to B, then yields.
- B receives the prompt on its next idle turn, does work, and replies.
- TermAl queues a resume prompt to A containing B's reply.

Two degenerate shapes fall out for free. A one-way handoff is an exchange with
`expectsReply: false` (A pushes context and does not yield on it). An open
back-and-forth is bounded ask/reply iterated until the connection's hop budget is
spent. v1 ships bounded ask/reply and one-way handoff; it does not add a separate
"channel" abstraction, because iterated exchanges already cover it with a
built-in stopping condition.

### Provenance Is Mandatory

A delivered exchange must announce that it came from a peer agent, not the human.
Without it, the receiver treats the message as a user prompt — it addresses
"you", asks clarifying questions into a transcript no human is reading, and may
take human-directed actions like committing. TermAl prepends the header; the
sender cannot forge or omit it:

```text
[from session-2787 · Claude · via connection-abc · hops left: 3]
<message body>

This message is from a peer agent session, not the user. Do not commit, push, or
ask the user questions on its behalf. To respond, call
termal_reply_to_session(exchange-xyz).
```

For a one-way ask (`expectsReply: false`) TermAl omits the reply instruction and
states that no reply is expected, so the recipient does not answer into an
exchange that is already terminal.

### Authority And Guardrails

Connections are created by the human in v1, through a `/connect-sessions` command
or the UI. The agent MCP surface exposes no connection-creation tool, so agents
may use edges that exist but cannot mint them. This keeps topology, blast radius,
and cost under human control, mirroring how delegations require an explicit spawn.
Agent-requested connections with human approval are a later addition, noted in
Open Questions.

The "human-only" property is enforced at the tool surface and UX, not as a hard
boundary. The create route lives on the same unauthenticated loopback API as the
rest of TermAl (see the security note below), so a shell-enabled session could
still `POST` it directly. Making human-only authoritative requires the create
route to demand a UI-scoped capability the agent process never receives — part of
the tracked loopback-auth work, not v1.

- **Hop budget.** Each connection carries `hopsRemaining`, decremented on every
  delivery including replies. When it reaches zero, further exchanges fail loudly
  to both sides. Loops between two agents are therefore structurally bounded, not
  bounded by heuristics or good behavior. Because a reply is itself a delivery, a
  reply-expecting ask reserves both hops atomically: an `expectsReply` ask
  requires at least two remaining hops and consumes them as a unit, so the budget
  can never strand a delivered exchange that is owed a reply it can no longer
  fund. A one-way ask (`expectsReply: false`) costs a single hop.
- **Delivery and reply deadlines.** An exchange to a busy peer queues behind its
  current turn through the existing queued-prompt path. A delivery TTL bounds the
  wait for the peer to go idle and consume the prompt. For a reply-expecting ask,
  a reply deadline also applies: if the peer's turn ends without calling
  `termal_reply_to_session`, the exchange fails at turn-end and the asker is
  resumed with a terminal exchange, so it can never yield forever. A one-way ask
  (`expectsReply: false`) has no reply deadline — it reaches its terminal
  `completed` state on delivery and the asker never yields on it.
- **Reviewer/read-only peers may reply, not initiate.** Same restriction class as
  nested reviewer spawning being disabled: a read-only session can answer a
  question but cannot drive another session. This is derived per endpoint from
  each session's own write policy at initiation time, not stored as one mode on
  the connection — an unordered pair with a single mode could not say which end is
  the restricted one.
- **Connect-time notice.** Both sessions receive a system notice when an edge is
  created; there is no tool to discover peers otherwise. `termal_list_connections`
  returns only the calling session's own edges, never a global session list — the
  same visibility boundary the delegation bridge already enforces.
- **No self-edges; pairs are unordered and de-duplicated.**

Security note: like the delegation bridge, this is an accounting and UX boundary,
not a sandbox. Any session with shell access can already reach
`POST /api/sessions/{id}/messages` on the unauthenticated loopback API. A
connection makes peer messaging first-class, auditable, and bounded; it does not
by itself stop an out-of-band session from injecting a prompt. Closing that gap
is loopback-auth work, tracked separately, and is not a prerequisite for v1.

### Command And Tool Surface

Internal commands and MCP tools mirror the delegation set, renamed for the peer
relationship. `resume_after_exchanges` is very close to
`resume_after_delegations`: it schedules a durable backend wait and yields rather
than polling in-turn.

```text
connect_sessions(sessionA, sessionB, options?) -> SessionConnection   // human/UI only
ask_session(connectionId, prompt, options?)    -> SessionExchange
reply_to_session(exchangeId, reply)            -> SessionExchange
resume_after_exchanges(exchangeIds, options?)  -> ExchangeWaitResponse
list_connections(sessionId)                    -> SessionConnection[]  // own edges only
close_connection(connectionId)                 -> SessionConnection
```

MCP tool names stay explicit and peer-scoped. Connection creation is deliberately
absent from the agent MCP surface in v1; edges come from the human.

```text
termal_list_connections
termal_ask_session
termal_reply_to_session
termal_resume_after_exchanges
```

Routes mirror the delegation endpoints:

```http
POST   /api/sessions/{sessionId}/connections          # create edge (UI/human)
GET    /api/sessions/{sessionId}/connections          # own edges
DELETE /api/connections/{connectionId}                # close edge
POST   /api/connections/{connectionId}/exchanges      # ask -> exchange id
POST   /api/exchanges/{exchangeId}/reply              # reply
POST   /api/sessions/{sessionId}/exchange-waits       # backend fan-in resume
```

`exchange-waits` reuses the delegation-wait mechanics: it schedules a
backend-owned resume prompt for the asker and is surfaced in `/api/state` and SSE
so a reload still shows a session waiting on a peer.

### Data Model

Connections are a separate record that reuses the wait/resume machinery rather
than overloading `DelegationRecord`. The two can be unified later behind a shared
`targetSessionId` + `ownsTarget` field (see Open Questions); keeping them
distinct for v1 avoids rewriting the delegation data model.

```typescript
type SessionConnectionStatus = "open" | "closed";

type SessionConnection = {
  id: string;
  // Unordered pair of existing top-level session ids; no self-edges.
  sessionIds: [string, string];
  hopsRemaining: number;            // shared budget, decremented per delivery
  status: SessionConnectionStatus;
  createdAt: string;
  createdBy: "human";               // v1: connections are human-created
};
// No `mode` field: a single mode on an unordered pair cannot identify which
// endpoint is restricted. Whether a given session may initiate an exchange is
// derived at ask time from that session's own write policy (a read-only/reviewer
// session may reply but not initiate).

type SessionExchangeStatus =
  | "queued"      // delivered to the target, awaiting its next idle turn
  | "delivered"   // consumed; a reply-expecting turn is in flight
  | "answered"    // reply captured; asker resumable (expectsReply: true)
  | "completed"   // one-way ask (expectsReply: false) delivered; no reply owed
  | "failed"      // target gone, hops exhausted, or connection closed
  | "expired";    // a delivery or reply deadline elapsed, or a reply-expecting
                  // peer turn ended with the exchange still unanswered

type SessionExchange = {
  id: string;
  connectionId: string;
  fromSessionId: string;
  toSessionId: string;
  prompt: string;
  expectsReply: boolean;            // false = one-way handoff
  status: SessionExchangeStatus;
  // Freeform prose. Deliberately NOT the reviewer `## Result` packet: peers
  // talk to each other, they do not file machine-parsed findings.
  reply?: string | null;
  createdAt: string;
  answeredAt?: string | null;
};
```

### Edge Cases

- **Target dies mid-exchange.** The asker is resumed with a `failed` exchange, not
  left waiting. Delegations already notify on child death; the only change is that
  `ownsTarget` is false, so TermAl does not tear the peer down.
- **Hop exhaustion.** Delivery fails and both sides are told. A silent drop is the
  worst outcome, because the asker would yield on a reply that can never arrive.
- **Connection closed mid-exchange.** Any pending exchange on that edge fails and
  resumes its asker.
- **Simultaneous mutual asks.** Both A and B ask before either replies. No lock is
  held, so both simply resume when answered; there is no deadlock, only two
  independent exchanges.
- **Remote/cross-machine peers.** Out of scope for v1. Reject an edge between
  sessions on different remotes with a clear error rather than half-routing it.

### Non-goals for v1

- No group or broadcast connections (>2 sessions); edges are strictly pairwise.
- No streaming or partial peer output; an exchange resolves to one reply.
- No agent-created connections; the human owns the topology.
- No cross-remote edges.
- No reuse of the reviewer `## Result` packet for peer replies.

## Implementation Phases

### Phase 1: Read-only Delegation Records

- Add `DelegationRecord` persistence.
- Add parent-child link metadata.
- Add API to spawn a read-only child session with a prompt.
- Add status/result endpoints.
- Add SSE deltas for delegation lifecycle.
- Add parent transcript card.

### Phase 2: Internal Tool Surface

- Expose spawn/status/result/cancel commands.
- Return compact result packets.
- Add optional wait semantics, including waiting on multiple delegation ids.
- Add timeout behavior that does not cancel by default.

### Phase 3: Agent MCP Bridge

- Add a TermAl-owned local MCP bridge that wraps the internal delegation command
  surface. Implemented as `delegation-mcp` over stdio.
- Launch the bridge per parent agent session with an implicit `parentSessionId`.
- Wire the bridge into ACP/Codex, Cursor, Gemini, and Claude session
  startup/resume paths.
- Keep delegation operations parent-scoped. (The v2 peer-messaging tools
  `termal_send_to_session` / `termal_list_sessions` are the deliberate exception:
  they reach root sessions across projects — see the v2 visibility boundary
  above.) Project/workspace opt-in and capability tokens are deferred until
  remote/shared transports need a stronger boundary — the same mechanism a hard,
  REST-level cross-session boundary would require.
- Add regression coverage for terminal status/result refresh, backend resume
  waits, and restart/reconcile behavior before relying on the bridge for review
  automation.

### Phase 4: Reviewer Batch UX

- Add helper command to spawn several read-only reviewers in parallel.
- Add grouped parent card.
- Add backend-scheduled result fan-in through parent resume waits, including a
  one-call reviewer-batch path.
- Keep UI result insertion human-driven unless the parent explicitly schedules a
  resume wait.
- Rewrite `/review-with-delegate` to require TermAl MCP tools and stop if they
  are absent.

### Phase 5: Worker Delegation

- Add explicit owned-path validation.
- Add optional isolated git worktree creation.
- Track changed files and verification commands.
- Add import/compare path for worktree diffs.

### Phase 6: Integration With Orchestration

- Let orchestration templates create delegation-like child groups.
- Allow delegation result packets to feed transition prompts.
- Reuse cards and lifecycle events.

### Phase 7: Peer Session Connections

Depends on Phase 1 records and the Phase 3 MCP bridge.

- Add `SessionConnection` and `SessionExchange` persistence and SSE deltas.
- Add `/connect-sessions` (human/UI) plus the exchange, reply, and exchange-wait
  routes.
- Reuse the delegation wait and queued-prompt resume for
  `resume_after_exchanges`.
- Enforce the hop budget, exchange TTL, the reviewer-cannot-initiate rule, and the
  TermAl-prepended provenance header.
- Add the peer MCP tools; keep connection creation off the agent surface.
- Draw the connection edge on both sessions' cards and a connect-time notice.

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
- a terminal status/result read refreshes the persisted delegation state and
  consumes any already-satisfied backend wait for that parent
- a backend restart while a wait is pending does not leave the parent blocked
  once all watched delegations are terminal
- a parent-scoped bridge cannot read, wait for, cancel, re-arm (follow up), or
  fetch result packets for a delegation owned by another parent session
- result is unavailable until completion
- cancel is idempotent

Agent MCP bridge:
- the bridge starts with an implicit parent session id; delegation tools expose
  no cross-parent listing, while the peer tools (`termal_list_sessions` /
  `termal_send_to_session`) reach root sessions only, never delegation children;
  `termal_send_to_session` additionally refuses to target the caller itself,
  while `termal_list_sessions` lists the caller
- each tool delegates to the existing API/command surface and preserves its
  validation errors
- no MCP tool accepts an arbitrary `parentSessionId`; the bridge process is
  launched with exactly one implicit parent
- child sessions remain normal TermAl sessions in storage, but parent-scoped MCP
  tools cannot enumerate, wait for, cancel, or fetch results for delegations
  outside that parent
- deleting a parent session cascades delegation cleanup, so routine reviewer
  accumulation is bounded by the parent lifecycle rather than by immediate
  child-session deletion after every review
- ACP/Codex, Cursor, and Claude startup paths can opt into the same bridge
  descriptor
- `/review-with-delegate` fails fast when the required TermAl MCP tools are
  missing
- long-running review waits use backend resume waits instead of shell polling

Isolation:
- read-only delegation either disables writes or clearly labels the policy as
  advisory when the selected agent runtime cannot enforce it
- worker mode requires canonicalized owned paths
- isolated worktree mode never writes into the main worktree directly
- changed-file auditing flags any write outside the declared owned paths

Peer connections:
- an exchange to an idle peer delivers, and the reply resumes the asker through a
  backend wait rather than in-turn polling
- an exchange to a busy peer queues behind its current turn instead of dropping,
  and expires through its TTL if the peer never goes idle
- hop-budget exhaustion fails the delivery and resumes both sides; it is never a
  silent drop
- a reviewer/read-only peer can reply but cannot initiate an exchange
- delivered exchanges carry the TermAl-prepended provenance header and cannot omit
  it
- simultaneous mutual asks resume both askers without deadlock
- closing a connection fails any pending exchange and resumes its asker
- `termal_list_connections` returns only the caller's own edges and no global
  session list
- a connection between sessions on different remotes is rejected with a clear
  error

## Acceptance Criteria

- A parent session can spawn a read-only child session from a bounded prompt.
- The parent transcript shows the child status.
- The child is a normal TermAl session and can be opened directly.
- The parent can retrieve a compact result packet after completion.
- The parent can continue work while the child runs.
- A canceled child leaves an auditable parent card and child transcript.
- Reload preserves delegation links and final results.
- No delegation path commits or pushes without explicit user action.
- Agent-facing delegated review is available through TermAl MCP tools rather
  than ad hoc shell polling or non-TermAl subagent systems.

## Open Questions

- Should `wait_delegations` stream incremental status or return only final state?
- Should result extraction be purely prompt-convention based in v1, or should
  the backend ask the child agent for a structured final packet?
- How much of isolated worktree setup should TermAl own versus delegating to the
  child agent?
- Should parent cards be regular transcript messages or session metadata rendered
  inline?
- Which remote/shared MCP transports, if any, need capability tokens or
  project-level opt-in beyond the current per-process local bridge?
- Should peer connections stay a distinct `SessionConnection`/`SessionExchange`
  record, or should delegations and connections unify behind a single
  `targetSessionId` + `ownsTarget` model?
- Should an agent be able to request a connection subject to human approval, or
  must every edge stay human-initiated?
- What are the right default hop budget and exchange TTL before either becomes a
  footgun or a source of false failures?
