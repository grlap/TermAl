# Feature Brief: Conversation Overview Map

## Status

Foundation in progress. The frontend now has a pure overview projection helper
and a first product rail for loaded long virtualized sessions. The rail derives
lightweight map items from loaded messages, optional virtualizer layout
snapshots, and marker inputs; renders clumped DOM segments for dense message
runs; shows marker pins and the active viewport; and supports click/drag
navigation through the virtualizer handle. Hover tooltips, marker filters, and
summary-only hydration jumps are still future work. Canvas rendering is a last
resort only if clumped DOM segments still profile poorly.

The conversation overview map is a zoomed-out, approximate view of a long
session transcript. It gives the user a fast spatial sense of the whole
conversation: where prompts happened, where long assistant outputs happened,
where commands/diffs/errors appeared, and where conversation markers sit.

This is a separate feature from Conversation Markers. Markers are durable
semantic anchors; the overview map is a navigation surface over the whole
transcript.

Related feature: [Conversation Markers](conversation-markers.md).

## Problem

Long TermAl sessions are hard to reason about from the normal chat viewport.
The user can scroll, search, or jump to bottom, but there is no compact map of
the conversation shape.

The screenshot/reference behavior is a zoomed-out transcript: not fully
readable, but enough to understand density, sections, and landmarks. That
should become a first-class navigation mode instead of forcing the user to
physically scroll through thousands of pixels.

## Goals

- Show a compact whole-session overview for long conversations.
- Make transcript structure visible: prompts, assistant responses, commands,
  diffs, approvals, errors, file changes, and markers.
- Support clicking/dragging in the overview to jump the main transcript.
- Use approximate layout and estimated heights when exact measurements are not
  available.
- Integrate with conversation markers as colored landmarks.
- Avoid rendering the full heavy transcript just to build the map.

## Non-goals for v1

- No readable miniature transcript.
- No exact pixel-perfect screenshot of the transcript.
- No expensive Mermaid/math/code rendering inside the overview.
- No editing from the overview.
- No independent transcript data model.
- No replacement for search or markers.

## Core Idea

Render a lightweight transcript map beside or over the normal conversation.
Each message becomes a simplified block whose height approximates the message's
real visual footprint.

Example visual encoding:

- user prompt: compact blue/neutral band
- assistant text: gray block sized by text length
- command: terminal-colored block with status accent
- diff: file/change-colored block
- approval/input request: attention-colored block
- error/failure: red accent
- marker: colored pin/stripe using marker color
- active viewport: translucent window over the map

The overview is a navigation approximation. It should be stable and useful even
when not all message heights have been measured.

## Data Model

The overview should be derived from existing session state and optional layout
measurements.

```ts
type ConversationOverviewItem = {
  messageId: string;
  messageIndex: number;
  type: Message["type"];
  author: Message["author"];
  estimatedHeightPx: number;
  measuredHeightPx?: number | null;
  status?: "running" | "success" | "error" | "approval" | null;
  markerIds: string[];
  textSample?: string;
};
```

Rules:

- `messageId` is the stable identity.
- `messageIndex` is a positioning hint for ordered layout.
- `estimatedHeightPx` is always available.
- `measuredHeightPx` refines the map when the message has been mounted or
  measured by the transcript virtualizer.
- marker overlays are joined by marker `messageId`.

## Estimation

The overview must not depend on exact transcript rendering.

Initial estimates:

- text/markdown: based on character count and line breaks
- command: based on command length, output line count, and status
- diff: based on changed-line count and file count
- approval/input: fixed card estimate
- file changes: based on file count
- parallel agents: based on agent count

Refinement:

- when `VirtualizedConversationMessageList` measures a page/message, publish
  measured heights to the overview cache.
- keep estimates for unmounted messages.
- avoid large scroll jumps when estimates are replaced by measurements.

## UI Placement

Possible placements:

- right-side minimap rail inside the session pane
- collapsible overlay opened from the transcript toolbar
- full "map mode" tab for very long sessions

Recommended v1:

- narrow right-side rail for sessions above a length threshold.
- click in the rail jumps the transcript.
- drag the viewport window to scrub through the transcript.
- hover shows a tooltip with message type, timestamp, marker names, and short
  preview.

## Marker Integration

Conversation markers should be first-class landmarks in the overview.

Behavior:

- show marker pins/stripes using marker color.
- show marker name on hover.
- clicking a marker pin uses the marker jump path.
- previous/next marker navigation can use the same resolved overview order.
- marker filters can dim unrelated overview items.

Markers remain their own data. The overview only projects them into map
coordinates.

## Navigation Semantics

Jumping from the overview should use the same infrastructure as marker jumps.

Flow:

1. Resolve clicked map y-coordinate to nearest message index.
2. Prefer exact `messageId` if the item is known.
3. Ask the transcript virtualizer to scroll to that message/index.
4. Hydrate the session first if the transcript is not loaded.
5. Highlight the target message briefly.

The overview should also reflect normal scrolling:

- active viewport rectangle tracks `scrollTop` and visible range.
- when virtualizer estimates change, update the viewport rectangle without
  stealing scroll focus.

## Performance Rules

- Do not render full Markdown, Mermaid, KaTeX, command output, or diffs in the
  overview.
- Do not mount hidden full message cards solely for the overview.
- Build the map from cheap summary data and existing measurements.
- Throttle scroll-to-overview synchronization.
- Cap tooltip preview length.
- Use canvas or a lightweight DOM list if the DOM version becomes expensive.

## Relationship To Existing Virtualization

The transcript virtualizer already owns message paging, measurement, and
scroll-position correction. The overview should not duplicate that machinery.

Recommended contract:

- virtualizer exposes a read-only layout snapshot:
  - message count
  - estimated total height
  - per-message/page estimated top/height
  - measured overrides when available
  - current viewport top/height
- overview sends navigation intents:
  - `jumpToMessageId(messageId)`
  - `jumpToMessageIndex(index)`

The overview should never write `scrollTop` directly except through the
virtualizer/session navigation API.

## Implementation Phases

### Phase 1: static overview rail

- Build the pure overview projection helper from loaded `session.messages`.
- Add deterministic tests for item classification, height projection, marker
  projection, and map-coordinate lookup.
- Build overview items from loaded `session.messages`.
- Render a compact right-side rail for long sessions.
- Show message-type blocks and marker pins.
- Click a block to jump to loaded messages.

### Phase 2: virtualizer integration

- Expose a layout snapshot from the conversation virtualizer.
- Track active viewport rectangle.
- Support jump to offscreen/unmounted message index.
- Hydrate before jumping when needed.

### Phase 3: interaction polish

- Add drag-to-scrub.
- Add hover tooltips.
- Add marker filters.
- Add keyboard shortcuts for overview focus and navigation.

### Phase 4: large-session scaling

- Clump dense message runs into a bounded number of DOM segments before
  considering canvas rendering.
- Move rail rendering to canvas only if segmented DOM still profiles poorly.
- Cache overview projections by session mutation stamp.
- Add incremental recompute for streaming updates.

## Testing Plan

Frontend:

- overview renders only above the long-session threshold.
- overview item heights are deterministic for known message shapes.
- clicking a map item requests jump to the correct message id/index.
- marker pins render with marker name and color.
- viewport rectangle updates after scroll.
- overview does not render heavy Markdown/Mermaid/math content.

Virtualizer integration:

- click an unmounted message and verify the virtualizer scrolls to it.
- replacement of estimates with measurements does not force a user-visible
  jump.
- hydrated session jump works from a summary-only state.

Accessibility:

- overview rail has an accessible label.
- marker pins expose marker name and kind.
- keyboard navigation can move between markers or overview blocks.

## Acceptance Criteria

- Long sessions show a zoomed-out overview rail or overlay.
- The overview displays approximate transcript structure without rendering full
  message content.
- The active viewport is visible in the overview.
- Clicking or dragging in the overview navigates the main transcript.
- Conversation markers appear as colored landmarks with names.
- Overview navigation works with virtualized/unmounted messages.
- The overview remains responsive during streaming updates.
