# Feature Brief: Conversation Markers

## Status

Exploratory. No product code implements this yet.

Conversation markers are durable, user-visible anchors inside an agent
conversation. They let the user mark important points in a long transcript,
name and color those points, jump between them, and optionally ask agents to
use them as context boundaries.

## Problem

TermAl conversations can become long quickly: streamed reasoning, tool output,
diffs, code review notes, approvals, retries, and parallel-agent summaries can
all land in the same transcript. The existing transcript is chronological, but
it has no first-class way to preserve the user's mental landmarks.

Common user needs:

- "This is where the plan changed."
- "Start from this point when continuing tomorrow."
- "Mark this as the review feedback we decided to fix."
- "Jump back to the last failing test output."
- "Remember this response as the accepted architecture decision."

Today the fallback is plain text in the conversation or an external note. Both
are lossy: they are hard to navigate, not typed, not stable under transcript
virtualization, and not available to future UI workflows.

## Goals

- Let users add named markers to specific conversation messages or message
  ranges.
- Make every marker visually identifiable through a required name and color.
- Show markers in the transcript without disrupting the chat flow.
- Provide previous/next marker navigation plus a marker list/timeline for
  quick navigation.
- Persist markers with the session so reloads and restarts keep them.
- Support marker types that are useful for agent workflows: checkpoint,
  decision, review, bug, question, handoff, and custom.
- Keep markers as metadata over the transcript, not extra assistant/user
  messages.

## Non-goals for v1

- No shared/team marker sync.
- No cross-device marker sync beyond the normal local session store.
- No automatic semantic marker generation by the model.
- No full task-management system.
- No marker comments or discussion threads in v1.
- No browser URL deep links in v1.

## Core Model

A marker is a durable annotation attached to a session and anchored to one or
more messages.

```ts
type ConversationMarkerKind =
  | "checkpoint"
  | "decision"
  | "review"
  | "bug"
  | "question"
  | "handoff"
  | "custom";

type ConversationMarker = {
  id: string;
  sessionId: string;
  kind: ConversationMarkerKind;
  name: string;
  body?: string | null;
  color: string;
  messageId: string;
  messageIndexHint: number;
  endMessageId?: string | null;
  endMessageIndexHint?: number | null;
  createdAt: string;
  updatedAt: string;
  createdBy: "user" | "agent" | "system";
};
```

Rules:

- `messageId` is the primary anchor.
- `messageIndexHint` is a recovery hint only. It is not authoritative.
- `name` is the short user-facing label shown in marker chips, lists, and
  navigation controls.
- `color` is required and should be stored as a theme token or validated CSS
  color, not inferred only from marker kind.
- Range markers use `endMessageId`; single-message markers leave it null.
- Markers survive message virtualization because they are resolved by message
  identity, not DOM position.
- Missing anchors degrade gracefully and remain visible in the marker list as
  unresolved.

## Marker Types

V1 should ship with a small fixed set:

- `checkpoint`: a user-defined resume point.
- `decision`: an accepted architectural/product decision.
- `review`: review feedback or review boundary.
- `bug`: a reproduced issue or failure point.
- `question`: an unresolved question to revisit.
- `handoff`: a point intended for a later agent or human handoff.
- `custom`: user-defined label with no special behavior.

Type-specific behavior should stay minimal in v1. The type mostly controls
icon, default color, default name suggestion, and filter grouping. The user can
override marker color without changing the marker kind.

## User Experience

### Creating a marker

Entry points:

- message overflow menu: `Add marker`
- selected message range: `Mark range`
- keyboard command from focused message
- optional slash command: `/marker <kind> <title>`

Creation form:

- kind picker
- name input
- color picker with kind-based default
- optional body/notes

Default name/color suggestions:

- checkpoint: `Checkpoint`, blue
- decision: first sentence of the selected assistant response, green
- review: `Review feedback`, amber
- bug: `Bug: <nearest error summary>`, red
- question: `Question`, violet
- handoff: `Handoff point`, cyan

### Rendering in the transcript

Markers should be visible but not noisy.

Recommended v1 rendering:

- small marker chip above or beside the anchored message
- marker icon, name, and color accent
- hover/focus reveals full body and actions
- range markers show a start chip and subtle end boundary
- active marker highlight uses the marker color for the chip and message
  outline

Actions:

- `Edit marker`
- `Copy marker link`
- `Jump to marker`
- `Delete marker`
- `Send from marker` or `Continue from marker` can be added after v1.

### Marker list

Add a compact marker list in the session side area or transcript toolbar.

List item fields:

- marker icon/color
- name
- kind
- timestamp
- short body preview

List behavior:

- click jumps to the anchor message
- filters by kind
- search by title/body
- unresolved anchors remain listed with a warning state

### Marker-to-marker navigation

The conversation toolbar should expose marker navigation independent of search.

Controls:

- `Previous marker`
- `Next marker`
- current marker name/color chip when the viewport is near or inside a marked
  range
- optional kind filter before navigation, e.g. jump only between `bug` markers

Ordering:

- sort markers by resolved `messageIndex`, then `createdAt`, then `id`.
- range markers occupy their start position for previous/next navigation.
- unresolved markers stay in the list but are skipped by previous/next until
  their anchor can be resolved.

Behavior:

- next marker after the bottom wraps to the first marker.
- previous marker before the top wraps to the last marker.
- navigation should update the active marker highlight without changing prompt
  history or session state.

### Navigation

Jumping to a marker must work with transcript virtualization.

Flow:

1. Resolve marker anchor by `messageId`.
2. If the message is currently loaded, scroll to it and highlight it.
3. If the transcript is summary-only or not loaded, hydrate the session first.
4. If the message is outside the mounted virtualized range, ask the
   virtualizer to scroll to the message index.
5. If the message cannot be found, show unresolved marker state.

## Backend Storage

Markers should persist with the session state in the same local persistence
domain as sessions.

Suggested Rust shape:

```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationMarker {
    id: String,
    session_id: String,
    kind: ConversationMarkerKind,
    name: String,
    body: Option<String>,
    color: String,
    message_id: String,
    message_index_hint: usize,
    end_message_id: Option<String>,
    end_message_index_hint: Option<usize>,
    created_at: String,
    updated_at: String,
    created_by: MarkerAuthor,
}
```

Storage options:

- V1 JSON persistence: store markers under the owning session.
- Future SQLite persistence: separate `conversation_markers` table keyed by
  `(session_id, marker_id)`.

Markers are metadata. They should not be inserted into `messages`, because
that would affect prompt history, transcript count, and agent-visible content.

## API Plan

Initial routes:

- `GET /api/sessions/{sessionId}/markers`
- `POST /api/sessions/{sessionId}/markers`
- `PATCH /api/sessions/{sessionId}/markers/{markerId}`
- `DELETE /api/sessions/{sessionId}/markers/{markerId}`

Optional later routes:

- `POST /api/sessions/{sessionId}/markers/{markerId}/jump-context`
- `GET /api/sessions/{sessionId}/markers/export`

Mutation behavior:

- marker mutations increment the global revision.
- marker mutations publish SSE updates.
- marker mutations do not mark the agent session active.
- marker mutations do not change message timestamps or message ordering.

## SSE And Delta Model

Marker changes should be delta-friendly.

```ts
type SessionMarkerDelta =
  | { type: "markerCreated"; sessionId: string; marker: ConversationMarker }
  | { type: "markerUpdated"; sessionId: string; marker: ConversationMarker }
  | { type: "markerDeleted"; sessionId: string; markerId: string };
```

Frontend application rules:

- Apply marker deltas even when `messagesLoaded === false`.
- Marker deltas update session summary metadata without requiring transcript
  hydration.
- If a marker references a missing message, keep the marker and mark it
  unresolved locally.

## Agent Integration

V1 markers are user-created, but they should be useful to agents.

Recommended prompt insertion actions:

- `Continue from marker`
- `Summarize since marker`
- `Review changes since marker`
- `Use marker as handoff context`

These actions should produce explicit user prompts that reference marker title,
session id, and anchor message id. They should not silently alter hidden
context.

Example handoff prompt:

```text
Continue from conversation marker "Accepted architecture decision"
in session session-42 at message msg-88. Use the marker note as the
handoff boundary and review messages after that point.
```

## Search And Filtering

Markers should integrate with transcript search as metadata results:

- search name/body text
- filter by kind
- jump from result to marker anchor
- include marker count in session summary only if cheap

Do not force full transcript hydration just to list markers. The marker list
should be available from session metadata.

## Persistence And Compatibility

Rules:

- Missing `markers` field means no markers.
- Unknown marker kind should load as `custom` with the original value preserved
  if practical.
- Deleting a message should not automatically delete markers in v1; it should
  mark them unresolved.
- Session deletion deletes its markers.

## Implementation Phases

### Phase 1: data model and static rendering

- Add marker types to backend and frontend.
- Add required marker `name` and `color` fields.
- Persist markers under sessions.
- Render marker chips on anchored messages.
- Add marker list with jump support for loaded transcripts.

### Phase 2: marker CRUD

- Add marker routes.
- Add create/edit/delete UI.
- Publish marker deltas over SSE.
- Add unresolved-anchor handling.

### Phase 3: virtualized transcript integration

- Add robust jump-to-message support through the conversation virtualizer.
- Add previous/next marker navigation over resolved marker anchors.
- Hydrate before jumping when needed.
- Highlight target message after jump.

### Phase 4: agent workflow actions

- Add `Continue from marker`, `Summarize since marker`, and
  `Use as handoff` actions.
- Add prompt insertion helpers.
- Add optional marker export.

## Testing Plan

Backend:

- marker create/update/delete persistence round-trip
- marker mutation increments revision
- marker delta serialization
- deleting a session removes markers
- missing marker returns 404

Frontend:

- marker chip renders on anchored message
- marker chip renders the configured name and color
- marker list filters by kind
- clicking a marker jumps to the message
- previous/next marker controls wrap and skip unresolved markers
- unresolved marker renders without crashing
- marker deltas apply while `messagesLoaded === false`
- marker edits preserve anchor identity

Virtualization:

- jump to marker outside mounted range
- jump after transcript hydration
- highlight target after virtualizer scroll settles

Agent workflow:

- `Continue from marker` inserts a deterministic prompt
- marker prompt includes session id, marker id, title, and anchor message id

## Acceptance Criteria

- A user can create a marker on a conversation message.
- A marker has a required user-visible name and color.
- The marker persists across reload.
- The marker is visible in the transcript and marker list.
- Clicking the marker list item scrolls to and highlights the anchored message.
- Previous/next marker controls jump between resolved markers in transcript
  order.
- Marker updates arrive live through SSE.
- Markers do not appear as normal transcript messages and do not alter prompt
  history.
- Missing anchors degrade gracefully instead of crashing the UI.
