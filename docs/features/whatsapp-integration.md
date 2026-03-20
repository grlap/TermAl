# Feature Brief: WhatsApp Integration

This document describes a summary-first WhatsApp integration for TermAl.

Backlog source: [`docs/bugs.md`](../bugs.md)

## Status

Not implemented. This is a proposed mobile relay layer on top of existing
project, session, and approval flows.

## Problem

TermAl's current UI is rich, desktop-oriented, and optimized for supervising
agent work with typed cards, split panes, diffs, files, and git state. A raw
mirror of that UI into WhatsApp would be noisy and hard to act on.

We need a mobile surface that helps a user stay in the loop without forcing
them back to the full workspace for every update.

## Core idea

Treat WhatsApp as a project status relay, not as a full TermAl client.

For each linked project, TermAl should send a short digest that answers three
questions:

- what was done
- what is the current project status
- what should happen next

The digest should include a small set of proposed actions based on the current
project state. Rich review still happens in TermAl itself.

## Why summary-first is the right shape

- WhatsApp is good at concise, asynchronous updates and simple actions.
- It is bad at streaming dense command output, large diffs, and complex
  workspace state.
- TermAl already has the raw ingredients for summaries: sessions, pending
  approvals, diffs, git status, and project routing.
- A summary-first model lets us reuse existing TermAl workflows without
  flattening every card type into awkward chat output.

## Goals

- Let a user monitor project progress from a phone.
- Send compact updates after meaningful work, not every intermediate event.
- Propose next actions based on the current status of the project.
- Let the user respond with a short instruction or choose a suggested action.
- Keep TermAl as the authoritative surface for detailed review.

## Non-goals for v1

- Full card-by-card mirroring of the desktop conversation.
- Token-by-token streaming over WhatsApp.
- Full workspace management from mobile.
- Multi-user routing, RBAC, or enterprise account management.
- Perfect natural-language planning from the relay itself.

## Current constraints

- TermAl is currently a local-first backend and should not be exposed directly
  as a public WhatsApp webhook target.
- Existing session APIs are already sufficient for a relay to create sessions,
  send prompts, observe updates, and submit approvals.
- Prompt attachments are image-only today, so WhatsApp documents, audio, and
  other media would need separate handling.
- WhatsApp actions are intentionally limited, so action suggestions must stay
  short and high-signal.

## User experience

### Project thread model

Each WhatsApp thread is linked to one TermAl project by default.

That thread becomes the user's mobile control channel for the project, not for
an arbitrary session. Internally, the relay can target the most relevant
session in that project or create one when needed.

### Outbound digest

A digest is sent when one of these happens:

- an agent finishes a meaningful turn
- a project reaches an approval-needed state
- new file changes land and the project becomes idle
- a scheduled heartbeat is due and there has been material progress since the
  previous digest

The default digest shape is:

```text
Project: termal
Status: waiting on your decision
Done: fixed the queued prompt bug, updated tests, and left the repo clean
Next: review the diff, approve the pending command, or ask the agent to commit
```

This should be short enough to read in a notification preview.

### Proposed actions

Each digest includes up to three suggested actions.

Examples:

- `Continue`
- `Review in TermAl`
- `Approve`
- `Reject`
- `Stop`
- `Ask agent to commit`
- `Keep iterating`

The action list should be state-driven rather than fixed.

### Inbound replies

The relay should support:

- quick-reply button taps mapped to common actions
- short free-text follow-ups that are forwarded into the active project session
- a few control phrases such as `status`, `stop`, `continue`, and `open`

Free-text replies should remain available, but the happy path should be action
selection from the digest.

## Digest model

The relay should synthesize a project digest from the latest TermAl state using
something like:

```ts
type ProjectDigest = {
  projectId: string;
  primarySessionId?: string | null;
  headline: string;
  doneSummary: string;
  currentStatus: string;
  proposedActions: ProposedAction[];
  deepLink?: string | null;
  sourceMessageIds: string[];
};

type ProposedAction = {
  id: string;
  label: string;
  prompt?: string;
  requiresConfirmation?: boolean;
};
```

The output should be concise and deterministic enough that the relay can send
it automatically without another LLM step in the loop.

## Status synthesis rules

Action suggestions should be derived from project state in priority order.

### 1. Approval pending

If any session in the project is waiting for approval:

- status should say approval is blocking progress
- actions should be `Approve`, `Reject`, and `Review in TermAl`

### 2. Error or failed verification

If the most recent turn ended in error or tests failed:

- summarize what failed
- actions should bias toward `Fix it`, `Open in TermAl`, or `Pause`

### 3. Diff ready for review

If the project has fresh edits and no approval is pending:

- summarize the changed files or goal completion
- actions should bias toward `Review in TermAl`, `Ask agent to commit`, or
  `Keep iterating`

### 4. Active work in progress

If an agent is still running:

- send only occasional heartbeat digests
- actions should bias toward `Stop`, `Open in TermAl`, or `Wait`

### 5. Idle project

If the project is idle and unblocked:

- summarize the most recent completed milestone
- actions should bias toward `Continue`, `Ask a question`, or `Open in TermAl`

## Proposed architecture

### 1. WhatsApp gateway

Run a small public-facing gateway that:

- receives inbound WhatsApp webhooks
- validates provider signatures
- maps a sender to a linked project
- talks to the existing TermAl REST and SSE APIs
- sends outbound digests and action messages back to WhatsApp

This keeps the public webhook boundary outside the current TermAl server.

### 2. Project digest builder

The gateway or TermAl backend needs a digest builder that collapses rich
session state into:

- a short done summary
- a current project status
- up to three proposed actions

The digest builder should prefer deterministic rules over a second LLM call.

### 3. Action dispatcher

Suggested actions should resolve to one of:

- a direct approval decision
- a canned prompt injected into the active project session
- a deep link back into TermAl

This keeps WhatsApp interactions shallow while still allowing user control.

### 4. Deep links back to TermAl

Many states should end with a link back to the full app.

Examples:

- review a diff
- inspect a failing command
- compare multiple sessions in a project
- read a long assistant explanation

WhatsApp should accelerate awareness and steering, not replace the main review
surface.

## API plan

The first pass can be implemented by a gateway using existing APIs, but a
cleaner backend contract would be:

### `GET /api/projects/{id}/digest`

Returns a compact project summary with:

- status text
- done summary
- proposed actions
- related session id
- optional deep link target

### `POST /api/projects/{id}/actions/{action_id}`

Executes a suggested action such as:

- continue
- stop
- approve
- reject
- ask-agent-to-commit

This avoids pushing too much state interpretation into the gateway.

### `GET /api/projects/{id}/events`

Optional later endpoint for project-scoped event aggregation if session-level
SSE becomes too low-level for the relay.

## Provider strategy

The transport layer should be provider-agnostic, but v1 should target one
provider first instead of trying to abstract multiple services immediately.

The provider adapter is responsible for:

- webhook verification
- inbound message normalization
- outbound text and action delivery
- contact and message id bookkeeping

## Implementation phases

### Phase 1: gateway and manual project linking

- add a small WhatsApp gateway service
- link one phone number to one project
- forward inbound text to the project's active session
- send one short digest when a turn completes

### Phase 2: deterministic digest builder

- synthesize `done`, `status`, and `next` from project state
- derive up to three proposed actions
- suppress low-signal updates and duplicate digests

### Phase 3: action execution and deep links

- support approve, reject, continue, stop, and commit-oriented actions
- add deep links back into TermAl for richer review

### Phase 4: polish

- scheduled heartbeat digests
- better project-level heuristics
- per-project notification preferences
- optional escalation rules for blocked work

## Testing plan

- backend tests for project digest synthesis across idle, active, approval, and
  error states
- gateway tests for sender-to-project routing and idempotent webhook handling
- integration tests for action dispatch into existing TermAl APIs
- snapshot tests for digest text and action suggestions
- manual tests on real WhatsApp threads for notification readability and
  action latency

## Acceptance criteria

- A linked project can send a concise WhatsApp digest after meaningful work.
- Each digest includes what was done and at least one proposed next action.
- Approval-blocked work produces approval-focused suggestions.
- Free-text replies from WhatsApp can steer the active project session.
- The user can jump from a digest back into the right TermAl project or
  session for full review.
- The WhatsApp thread remains readable without mirroring every raw card.
