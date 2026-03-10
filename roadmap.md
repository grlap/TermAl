# TermAl Roadmap

This document tracks product phases. It is intentionally separate from the `P0` / `P1` / `P2`
implementation backlog in [`docs/bugs.md`](/Users/greg/GitHub/Personal/termal/docs/bugs.md).

For product framing, see [`docs/vision.md`](/Users/greg/GitHub/Personal/termal/docs/vision.md).

Priority buckets answer "what should we build next inside the current product shape?"

Phases answer "what product are we actually shipping over time?"

## Product Direction

TermAl evolves in four phases:

1. Phase 1: local AI terminal
2. Phase 2: remote PC access
3. Phase 3: mobile access
4. Phase 4: remote pair programming

The rule is simple: each phase builds on the previous one.

- Phase 2 does not ship until Phase 1 is solid.
- Phase 3 does not ship until the remote foundation from Phase 2 is stable.
- Phase 4 does not ship until TermAl is already a strong remote supervision tool on desktop and mobile.

## Product Principle

TermAl should be built from the ground up for working with coding agents.

That means:

- agent-native message types, not raw terminal text as the primary UI
- explicit approvals, review flows, and diff inspection
- session, queue, and review models designed for long-running agent work
- remote workflows designed around supervising, steering, and collaborating with agents

The product should not become "a normal terminal app plus some AI buttons." The structure needs to
serve agent workflows directly.

## Phase 1: Local AI Terminal

### Goal

Ship a reliable local control room for AI coding sessions where the backend server and UI run on
the same machine as the agent processes.

### User promise

"I can run Claude, Codex, and later Gemini on my own machine, inspect their work in a structured
UI, approve actions, review diffs, and manage multiple sessions without touching raw terminal
streams."

### Core architecture

- Rust backend runs locally
- React UI runs locally
- Agent runtimes run locally as child processes
- Persistence is local-only
- No relay, no remote sync, no mobile client

### Scope

- Multi-session workspace
- Claude and Codex integration
- Gemini integration
- Streaming responses
- Approvals and stop controls
- Structured cards for text, commands, diffs, markdown, and approvals
- Prompt queueing
- Diff preview tabs and backlinks
- Saved review comments and agent review replies
- Reliability, restart safety, and test coverage

### Not in scope

- Access from another computer
- Background daemon for machine registration
- Hosted relay
- Push notifications
- Touch-first UI

### Exit criteria

- Core agent integrations are stable enough for daily use
- Session persistence and queued work survive restart correctly
- Diff review workflow is usable end to end
- Major local-only reliability issues are closed
- The local app architecture is stable enough to sit behind a remote transport layer

## Phase 2: Remote PC Access

### Goal

Let the user access the same TermAl sessions from another computer while the agent processes keep
running on the original machine.

### User promise

"I can leave my development machine running and open the same TermAl workspace from another laptop
or desktop without exposing raw agent processes directly to the internet."

### Core architecture shift

Phase 2 adds network transport, but should not replace the Phase 1 session and rendering model.

- Local machine runs an agent daemon or background service
- Daemon manages local agent runtimes
- Daemon connects outbound to a relay
- Remote desktop client connects to the relay
- Relay routes events and commands; it should stay thin

### Scope

- Remote authentication
- Machine registration
- Relay server
- Outbound machine connection model
- Remote desktop web client
- Session list and chat view over remote transport
- Remote approvals, stop, queue, and diff review flows
- Basic reconnect behavior and session continuity

### Not in scope

- Full mobile product polish
- Offline-first mobile behavior
- Multi-user collaboration
- Complex enterprise auth

### Exit criteria

- User can connect to a remote machine from another PC
- Session streaming, approvals, queued prompts, and diff review work remotely
- Relay design is thin and operationally simple
- Remote transport does not force a rewrite of Phase 1 message/session concepts

## Phase 3: Mobile Access

### Goal

Make TermAl usable from a phone as a first-class remote companion, not just a squeezed desktop UI.

### User promise

"I can check on long-running coding sessions, review diffs, approve or reject actions, and respond
to review threads from my phone."

### Core architecture shift

Phase 3 reuses the remote transport from Phase 2 and adds a mobile-optimized client.

- Reuse relay and machine-daemon model from Phase 2
- Add mobile-specific UI and interaction design
- Add notification strategy for long-running sessions and approval requests

### Scope

- Mobile client, likely PWA first and native later if needed
- Touch-optimized session list and conversation UI
- Mobile-friendly approval flow
- Mobile diff preview and review comments
- Notifications for turn completion and approval requests
- Small-screen navigation for session, diff, and source views

### Not in scope

- Full code editing from mobile
- Heavy source browsing parity with desktop
- Advanced multi-pane workspace layout

### Exit criteria

- Mobile user can monitor and manage active sessions without desktop fallback
- Approval and diff review flows are practical on a phone
- Notifications are reliable enough to make mobile useful for supervision

## Phase 4: Remote Pair Programming

### Goal

Enable real remote pair programming around agent-driven work, where two humans can collaborate on
the same session and steer the same coding agent together.

### User promise

"I can bring another engineer into the same TermAl workspace, review what the agent is doing
together, leave feedback, and coordinate approvals and next steps in real time."

### Core architecture shift

Phase 4 adds multi-user collaboration on top of the remote transport from Phase 2 and the mobile
access patterns from Phase 3.

- Shared session presence
- Shared review state
- Shared approval context
- Multi-user event routing and identity
- Collaboration semantics instead of single-user remote control

### Scope

- Presence indicators for who is viewing or controlling a session
- Shared cursor or selection equivalents where useful
- Shared diff review threads
- Multi-user approval and handoff flows
- Per-user identity on comments, replies, and actions
- Session-level activity feed for collaborative work

### Not in scope

- Full Google-Docs-style realtime code editing
- General team chat platform features
- Broad project management workflows unrelated to active agent sessions

### Exit criteria

- Two users can observe and steer the same remote session without ambiguity
- Review comments and agent replies support multi-user collaboration cleanly
- Approval flows make it clear who approved, rejected, or delegated an action
- The collaboration model feels purpose-built for AI-assisted pair programming, not like a thin
  screen-share replacement

## Ordering Rules

These rules prevent roadmap drift:

1. Do not start relay-first work before the local product is dependable.
2. Do not start mobile-specific polish before remote PC access is stable.
3. Keep the message model, session model, and review model transport-agnostic so Phase 2 and Phase
   3 add connectivity, not a second product.
4. Do not treat Phase 4 as generic collaboration bolted onto the side; it should deepen the
   agent-native workflow rather than dilute it.

## Mapping Current Work To Phases

Most current work in [`docs/bugs.md`](/Users/greg/GitHub/Personal/termal/docs/bugs.md) belongs to
Phase 1.

Examples:

- Gemini integration is Phase 1
- Codex app-server coverage is Phase 1
- Runtime reliability and restart safety are Phase 1
- Diff preview tabs are Phase 1
- Saved review comments and agent replies are Phase 1
- Relay, daemon, and remote auth are Phase 2
- Mobile client and notifications are Phase 3
- Multi-user presence and collaborative remote session control are Phase 4

## Current Status

As of March 10, 2026, TermAl is still in Phase 1.

The current app is already a local Rust server plus React UI, but the remaining work is still
primarily Phase 1 work: integration depth, reliability, structured review flows, and local UX
polish.
