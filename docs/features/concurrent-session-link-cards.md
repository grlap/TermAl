# Feature Brief: Concurrent Session Link Cards

Backlog source: proposed feature brief; not yet linked from `docs/bugs.md`.

This brief describes a message type that represents another agent session
linked from the current conversation. The linked sessions remain fully
disjoined; the card is purely a graphical representation, a UI convenience
for keeping related work visible in one place.

## Problem

TermAl already has first-class sessions, workspace tabs, and several message
types for things that happen inside a session. What it does not have is a clean
way for one conversation to say:

- "I spawned another agent session"
- "That other session is still running"
- "Open that session in its own tab"
- "Show me just enough of that session here without duplicating its transcript"

And no way for a user to say:

- "I have this other session running — let me drag it in here so I can keep
  an eye on it without switching tabs"
- "I want all related work visible in one place"

Today the closest fallback is plain assistant text, but that loses structure:

- no stable link to the other session
- no live status
- no one-click navigation
- no clear distinction between historical narration and a real concurrent task

## Core idea

Add a new message type that links to another real session.

The current conversation keeps a compact card pointing to the linked session.
The linked session remains a normal first-class session with its own history,
status, commands, diffs, approvals, and tabs. The two sessions are fully
disjoined — they share no state and know nothing about each other. The card
is purely a graphical shortcut.

This is a linked-session model, not a nested transcript model.

Three entry paths produce the same card:

1. **Spawn**: an agent kicks off a new session from the current one. The card
   appears automatically so you can check back when the work is done.
2. **Drag in**: drag an existing session from the session list (or another
   tab) into the current conversation. A link card is inserted so you can
   monitor that session's progress without switching tabs.
3. **Prompt**: the user types a TermAl-native `/async` command in the
   composer. TermAl creates a new linked session, starts it with the provided
   prompt, and inserts the card into the current conversation.

## Conversation model

The linked session is a whole conversation, not a snippet. The two sessions
are fully disjoined — there is no ownership relationship, no shared state, and
no coupling beyond the card that references a `sessionId`.

That means:

- the linked session can keep running after the card message is rendered
- the linked session can be moved into its own workspace tab at any time
- the card only reads summary data from the linked session when it needs
  inline context
- the card never stores a copied transcript of the linked session
- either session can be closed or removed independently

## Why linked sessions are the right shape

The card should not embed a copy of the linked session conversation.

If the full linked transcript is duplicated into the current conversation:

- there are now two sources of truth
- live status can drift
- rendering gets heavier as nested conversations grow
- moving the linked session into its own tab becomes awkward because the
  current conversation already
  contains a stale copy

If the card only stores link metadata:

- the linked session stays authoritative
- the card can render live status by reading current session state
- the card can render a live summary by reading the linked session preview
- the linked session can always be opened directly in a pane or tab
- the current conversation still preserves the historical event that the
  linked session was created

## User experience

### Automatic card (spawn flow)

When an agent starts concurrent work, the current session receives a compact
message card automatically, for example:

- `Spawned parallel Codex session`
- `Reviewing git status in /Users/greg/GitHub/Personal/termal`

The card should show:

- agent badge
- linked session name
- workdir
- live status chip: `active`, `idle`, `approval`, or `error`
- current linked session preview text
- actions:
  - `Open session`
  - `Open in new tab`

### Manual card (drag-in flow)

The user drags an existing session from the session list or another tab into
the current conversation. A link card is inserted at the current scroll
position. This lets the user assemble a single conversation view that tracks
all the related work they care about.

The dragged session is not moved or modified — only a link card is created.

### Prompt-created linked session (`/async` flow)

The composer should also be able to create a linked session directly.

Examples:

- `/async compare our solution to the one used in ScyllaDB`
- `/async --codex do code review`

Recommended behavior:

- `/async` is handled by TermAl, not forwarded verbatim to Claude or Codex
- submitting it creates a new linked session immediately
- the current conversation receives a `concurrentSession` card right away
- the new session starts running with the remaining prompt text
- the card can then be used to monitor progress inline or open the session in
  a tab

Recommended v1 flags:

- `--codex`: start the linked session as Codex
- `--claude`: start the linked session as Claude

Recommended default if no agent flag is provided:

- use the same agent type as the current session

Open questions for later:

- optional `--model <name>`
- optional `--workdir <path>`
- optional `--title <text>`
- whether `/async` should support attaching files from the current draft

### Linked session

The linked session is just a normal session:

- it stays in the session list
- it can be opened in any workspace pane
- it can keep streaming independently
- it can outlive the session that references it

### Reading inline vs opening separately

This design supports both behaviors the user asked for:

- read what is needed inline from the card
- move the concurrent work into its own tab when deeper inspection is needed

The inline card should stay summary-only. Detailed reading belongs in the
linked session itself.

Recommended inline summary sources, in order:

- the linked session's existing `preview` field if present
- otherwise the latest meaningful summary already exposed through session list
  state
- never a second copy of the full linked transcript inside the current
  conversation

## Proposed message type

Frontend:

```ts
type ConcurrentSessionMessage = BaseMessage & {
  type: "concurrentSession";
  sessionId: string;
  agent: AgentType;
  title: string;
  detail?: string | null;
  workdir: string;
  originMessageId?: string | null;
};
```

Backend:

```rust
Message::ConcurrentSession {
    id: String,
    timestamp: String,
    author: Author,
    session_id: String,
    agent: Agent,
    title: String,
    detail: Option<String>,
    workdir: String,
    origin_message_id: Option<String>,
}
```

Notes:

- `sessionId` is the critical field. It links to the real linked session.
- `title` is the concise action summary shown on the card.
- `detail` is optional human-readable context.
- `workdir` is copied into the message so the historical record remains useful
  even if the linked session is later removed.
- `originMessageId` is optional metadata for tracing which message created the
  link.

## Rendering rules

The UI should resolve live state for the linked session from the global
`sessions` array.

That means the card should:

- look up the referenced session by `sessionId`
- render the linked session's current `status`
- render the linked session's current `preview`
- degrade gracefully if the linked session no longer exists

Missing-session fallback:

- show the historical title/detail/workdir from the message itself
- replace the live chip with something like `Unavailable`
- keep navigation disabled instead of crashing

This split should stay explicit:

- message payload = durable historical record
- live session lookup = current status and preview

## Workspace behavior

The card should integrate with the current workspace model the same way any
other session-opening action does.

Primary actions:

- `Open session`: focus an existing tab for that session, or open it in the
  active pane if none exists
- `Open in new tab`: explicitly create/focus a separate session tab in the pane

Because the linked target is already a real session, no special nested pane
model is needed.

If a session is already open somewhere, `Open session` should prefer focusing
the existing tab instead of creating duplicates.

## Backend changes

- Add a new `ConcurrentSession` variant to `Message`.
- Add serialization/deserialization support in persisted session state.
- Add a helper to append a concurrent-session message when a linked session is
  created from another session (spawn flow).
- Add a command/route for inserting a link card into an existing session by
  `sessionId` (drag-in flow).
- Add a command/route for creating a linked session from a prompt plus source
  session context (`/async` flow).

Open question:

- Should the session creation API accept `sourceSessionId` and automatically
  add the link card to the source conversation, or should the caller create the
  linked session and then post a separate link message?

Recommended v1:

- For the spawn flow: allow `sourceSessionId` on session creation. Backend
  creates the linked session and appends the link card to the source
  conversation in one operation. That avoids partial state where the linked
  session exists but the source conversation never records the relationship.
- For the drag-in flow: a separate "insert link card" command that takes a
  target `sessionId` and appends the card to the current session. No session
  creation involved.
- For the `/async` flow: parse the local command before agent dispatch, create
  the linked session, append the card to the current conversation, and send
  the remaining prompt text to the new session as its first turn.

Recommended request shape for `/async`:

```ts
type CreateLinkedSessionRequest = {
  sourceSessionId: string;
  agent?: "claude" | "codex";
  prompt: string;
  model?: string;
  workdir?: string;
  title?: string;
};
```

This should be a TermAl-native path, not a message that gets passed through to
the underlying agent runtime unchanged.

## Frontend changes

- Extend `Message` in `ui/src/types.ts` with `ConcurrentSessionMessage`.
- Add a `ConcurrentSessionCard` component in the message renderer.
- Pass session lookup or an `onOpenSession` callback into the card renderer.
- Reuse existing workspace open/focus behavior instead of inventing a new
  navigation path.
- Detect local `/async` commands in the composer before normal message send.
- Create the linked session through a dedicated app API instead of sending the
  raw `/async ...` text to the active session.
- Add drag-and-drop from the session list or tab strip into the conversation so
  users can embed session cards graphically.

Card behavior:

- render stable historical fields from the message
- merge in live session status/preview when the referenced session exists

Minimal renderer inputs:

- the `ConcurrentSessionMessage`
- session lookup by `sessionId`
- callbacks for `Open session` and `Open in new tab`

## Use cases

- **Spawn and check back**: kick off an agent to investigate something (e.g.
  failing tests, repo status). Keep working in the current session. Glance at
  the card when the linked session finishes to read the findings.
- **Prompt-created side quest**: type `/async compare our solution to the one
  used in ScyllaDB` and let a second session research it while you continue the
  main conversation.
- **Prompt-created review**: type `/async --codex do code review` to start a
  parallel Codex review without leaving the current session.
- **Second opinion**: spawn a session with a different model to review the same
  code or proposal. Compare findings side by side without leaving your main
  conversation.
- **Assemble related work**: drag several existing sessions into one
  conversation to create a dashboard-like view of everything you're tracking.
  No tab-switching needed.
- **Code review**: start a review session, then drag it into the session where
  you're working on the code so you can see review comments inline while you
  fix them.

## Suggested UX copy

Examples:

- `Parallel session`
- `Spawned Codex session`
- `Spawned Claude review session`

Possible subcopy:

- `Reviewing repo status in /Users/greg/GitHub/Personal/termal`
- `Waiting for approval in the linked session`
- `Finished: summarized failing tests`

## Non-goals for v1

- No embedded linked transcript inside the current conversation card
- No nested scrolling conversation inside a message card
- No automatic transcript mirroring between sessions
- No multi-session orchestration graph yet
- No special scheduling semantics beyond linking one session to another
- No arbitrary shell-like process control syntax beyond a small `/async`
  command surface

## Future extensions

Once the basic linked-session card exists, it can grow into richer coordination
features:

- session relationship trees
- "follow live" mode from the parent card
- aggregated territory/activity summaries
- grouped cards for several concurrently spawned sessions
- explicit handoff states such as `spawned`, `watching`, `blocked`, `done`

## Recommended v1 plan

1. Add the new message type in backend and frontend types.
2. Render a compact card that links to a real `sessionId`.
3. Show live status and preview by looking up the referenced session.
4. Add `Open session` and `Open in new tab` actions.
5. Add backend support for creating a linked session from a parent in one call
   (spawn flow).
6. Add drag-in support: drop a session from the session list or another tab
   into the current conversation to insert a link card (drag-in flow).
7. Add a TermAl-native `/async` composer path for creating a linked session
   from a prompt, with `--claude` and `--codex` as the initial explicit
   switches.

That delivers the main value quickly without introducing duplicated transcript
state or a more complex nested conversation model.
