# Feature Brief: Mobile Notifications & Remote Steering

This document describes a transport-agnostic mobile notification and steering
layer for TermAl, replacing the earlier WhatsApp-only proposal.

Backlog source: [`docs/bugs.md`](../bugs.md)

## Status

Not implemented. This is a proposed mobile relay layer on top of existing
project, session, and approval flows.

## Problem

TermAl's current UI is rich, desktop-oriented, and optimized for supervising
agent work with typed cards, split panes, diffs, files, and git state. A raw
mirror of that UI into a mobile notification channel would be noisy and hard
to act on.

We need a mobile surface that helps a user stay in the loop without forcing
them back to the full workspace for every update.

## Core idea

Treat the mobile channel as a project status relay, not as a full TermAl
client.

For each linked project, TermAl should send a short digest that answers three
questions:

- what was done
- what is the current project status
- what should happen next

The digest should include a small set of proposed actions based on the current
project state. Rich review still happens in TermAl itself.

## Why summary-first is the right shape

- Mobile channels are good at concise, asynchronous updates and simple actions.
- They are bad at streaming dense command output, large diffs, and complex
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
- Token-by-token streaming over a mobile channel.
- Full workspace management from mobile.
- Multi-user routing, RBAC, or enterprise account management.
- Perfect natural-language planning from the relay itself.

## Transport evaluation

The digest model and status synthesis rules are transport-agnostic. The
transport layer is the delivery mechanism. Here is a comparison of options
ordered by fit for TermAl's single-user local-first Phase 1:

### Phase 0: PWA push notifications (recommended first)

| Dimension       | Assessment |
|-----------------|------------|
| Cost            | Free — no external service |
| Setup           | Add a service worker + VAPID keys to the existing TermAl web frontend |
| Approval        | None — browser permission grant only |
| Actions         | Notification action buttons (Approve / Open / Stop) |
| Bidirectional   | No — one-way push only, tap opens TermAl |
| Privacy         | No third-party involvement at all |
| Limitation      | Requires the browser to be installed and permission granted; no conversation thread |

Since TermAl is already a web app, this is the lowest-friction option. The
backend emits a Web Push message when project state changes meaningfully, and
the browser shows a native notification with action buttons. iOS supports PWA
push since 16.4.

### Phase 1: Telegram bot (recommended for bidirectional)

| Dimension       | Assessment |
|-----------------|------------|
| Cost            | Free — no per-message fees, no BSP |
| Setup           | Create bot via @BotFather, get token — done in 60 seconds |
| Approval        | None — no template review, no business verification |
| API             | Simple HTTP REST; polling or webhooks; inline keyboard buttons built-in |
| Actions         | Inline keyboard buttons map directly to Approve / Reject / Continue |
| Bidirectional   | Yes — full conversation thread with free-text and button replies |
| Deep links      | `t.me/yourbot?start=project-xyz` works natively |
| Rate limits     | 30 msgs/sec to different users — more than enough for a single developer |
| Gateway         | Not needed — Telegram handles delivery; TermAl just POSTs to the Bot API |

The entire gateway described in the original WhatsApp doc can be replaced by a
~200-line adapter that polls the Telegram Bot API or listens for webhooks via
an ngrok/cloudflare tunnel. No Meta business verification, no template
approval, no per-message cost.

### Phase 2: ntfy (alternative self-hosted push)

| Dimension       | Assessment |
|-----------------|------------|
| Cost            | Free — self-hosted or use ntfy.sh |
| Setup           | One HTTP call: `curl -d "message" ntfy.sh/your-topic` |
| Actions         | Supports action buttons in notifications (open URL, HTTP request) |
| Bidirectional   | No — one-way push only |
| Privacy         | Fully self-hosted option, no third-party accounts needed |
| Limitation      | No conversation thread; better for "tap to open TermAl" than steering |

Good for users who want self-hosted notifications without any external
accounts. Can coexist with Telegram or PWA push.

### Phase 3: WhatsApp (later, for teams)

| Dimension       | Assessment |
|-----------------|------------|
| Cost            | $0.01–$0.13 per message depending on country + BSP markup |
| Setup           | Meta Business verification (days/weeks), BSP account, phone number lockdown |
| Approval        | Every outbound digest template must be pre-approved by Meta |
| 24-hour window  | After 24h without user reply, only pre-approved templates can be sent |
| Gateway         | Public webhook endpoint + signature validation + tunnel to local TermAl |
| Bidirectional   | Yes — but constrained by templates and 24-hour windows |
| Advantage       | Ubiquitous; natural fit for teams already using WhatsApp for coordination |

WhatsApp is the right choice when TermAl moves to multi-user or team
scenarios where WhatsApp is the existing coordination channel. The overhead is
not justified for single-user local-first use.

## Current constraints

- TermAl is currently a local-first backend and should not be exposed directly
  as a public webhook target without a tunnel or relay.
- Existing session APIs are already sufficient for a relay to create sessions,
  send prompts, observe updates, and submit approvals.
- Prompt attachments are image-only today, so media from mobile channels would
  need separate handling.
- Mobile actions are intentionally limited, so action suggestions must stay
  short and high-signal.

## User experience

### Project thread model

Each notification channel is linked to one TermAl project by default.

That channel becomes the user's mobile control surface for the project, not
for an arbitrary session. Internally, the relay can target the most relevant
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

### Inbound replies (Telegram / WhatsApp only)

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

```
┌──────────────┐      SSE       ┌──────────────────┐   HTTP   ┌─────────────┐
│  TermAl      │ ──────────────>│  Transport       │ ────────>│  Telegram   │
│  Backend     │<───── REST ────│  Adapter         │<─────────│  Bot API    │
│  :6543       │                │  (small process) │          │  (or PWA /  │
└──────────────┘                └──────────────────┘          │   ntfy)     │
                                         │                    └─────────────┘
                                         │                          │
                                         │    push digests          │
                                         │    inline buttons        │
                                         └──────────────────────────┘
                                                    │
                                             ┌──────▼──────┐
                                             │  Your Phone │
                                             └─────────────┘
```

The adapter subscribes to TermAl's SSE stream (`/api/events`) for state
changes, builds a digest when something meaningful happens, sends it to the
transport with action buttons, and maps inbound replies back to TermAl REST
calls (approve, send prompt, stop).

### 1. Digest builder (backend)

The TermAl backend needs a digest builder that collapses rich session state
into:

- a short done summary
- a current project status
- up to three proposed actions

The digest builder should prefer deterministic rules over a second LLM call.
This logic lives in the backend regardless of transport.

### 2. Transport adapter

A thin adapter per transport that:

- receives inbound messages (Telegram webhook, PWA service worker message)
- maps a sender/topic to a linked project
- calls the digest and action APIs
- sends outbound digests and action buttons in the transport's native format

### 3. Action dispatcher

Suggested actions should resolve to one of:

- a direct approval decision
- a canned prompt injected into the active project session
- a deep link back into TermAl

This keeps mobile interactions shallow while still allowing user control.

### 4. Deep links back to TermAl

Many states should end with a link back to the full app.

Examples:

- review a diff
- inspect a failing command
- compare multiple sessions in a project
- read a long assistant explanation

The mobile channel should accelerate awareness and steering, not replace the
main review surface.

## API plan

The first pass can be implemented by a transport adapter using existing APIs,
but a cleaner backend contract would be:

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

This avoids pushing too much state interpretation into the transport adapter.

### `GET /api/projects/{id}/events`

Optional later endpoint for project-scoped event aggregation if session-level
SSE becomes too low-level for the relay.

## Implementation phases

### Phase 0: PWA push notifications

- add a service worker to the TermAl frontend with VAPID key registration
- emit Web Push messages from the backend when a project reaches a meaningful
  state change (turn complete, approval needed, idle with changes)
- include notification action buttons for common actions (Open, Approve, Stop)
- no external service, no account, no cost

### Phase 1: Telegram bot adapter

- add a small Telegram bot adapter (polling or webhook)
- link one Telegram chat to one project
- forward inbound text to the project's active session
- send one short digest with inline keyboard buttons when a turn completes
- support approve, reject, continue, stop actions via button taps

### Phase 2: deterministic digest builder

- synthesize `done`, `status`, and `next` from project state
- derive up to three proposed actions
- suppress low-signal updates and duplicate digests

### Phase 3: WhatsApp adapter (optional, for teams)

- add a WhatsApp Business API adapter via a BSP (Twilio or similar)
- Meta business verification and template approval
- same digest model and action dispatcher as Telegram
- justified only when TermAl serves multi-user or team workflows

## Transport comparison

| Dimension              | PWA Push          | Telegram Bot      | ntfy              | WhatsApp Business |
|------------------------|-------------------|-------------------|-------------------|-------------------|
| Cost                   | Free              | Free              | Free              | $0.01–$0.13/msg + BSP |
| Setup time             | Minutes           | 60 seconds        | Minutes           | Days–weeks        |
| Approval / verification | Browser permission | None              | None              | Meta Business verification + template approval |
| Action buttons         | Notification actions | Inline keyboard  | URL / HTTP actions | Quick replies only |
| Bidirectional          | No (tap opens app) | Yes (full thread) | No (push only)    | Yes (24h window)  |
| Edit sent messages     | No                | Yes               | No                | No                |
| Free-text replies      | No                | Yes               | No                | Only within 24h   |
| External gateway       | No                | No (long polling) | No                | Yes (public HTTPS) |
| Change message format  | Instant           | Instant           | Instant           | Resubmit template |
| Best for               | Phase 0 — zero deps | Phase 1 — full steering | Self-hosted push | Teams / multi-user |

### Phase 4: polish

- scheduled heartbeat digests
- better project-level heuristics
- per-project notification preferences and transport selection
- optional escalation rules for blocked work

## Testing plan

- backend tests for project digest synthesis across idle, active, approval, and
  error states
- transport adapter tests for sender-to-project routing and idempotent message
  handling
- integration tests for action dispatch into existing TermAl APIs
- snapshot tests for digest text and action suggestions
- manual tests on real Telegram threads and PWA notifications for readability
  and action latency

## Acceptance criteria

- A linked project can send a concise mobile digest after meaningful work.
- Each digest includes what was done and at least one proposed next action.
- Approval-blocked work produces approval-focused suggestions.
- Bidirectional channels (Telegram) allow free-text replies to steer the
  active project session.
- The user can jump from a digest back into the right TermAl project or
  session for full review.
- The mobile thread remains readable without mirroring every raw card.
