# Response Board

The Response Board is one durable, human-facing card collection with one
shared staging inbox and lightweight inner tabs for separate spatial canvases.
It is separate from the agent [Coordination
Board](agent-boards.md): the coordination board stores small versioned JSON
facts for agents, while the Response Board renders immutable transcript-card
snapshots for a person.

## Opening and pinning

- **Open Response Board** in the left control surface opens or focuses a board
  workspace tab in that pane. Two panes can display different inner tabs.
- A transcript message can be pinned with **Pin to board** or dragged from its
  metadata header. The header is the drag handle so text in the message body
  remains selectable; the drag preview still shows the complete message card.
- Drag payloads carry only `sessionId` and `messageId`. The server resolves the
  durable transcript row and creates the snapshot. Browser-supplied message
  bodies are never accepted as board content.
- Cards retain their content if the source transcript is later pruned. Their
  source ids and durable message position remain available for **Open in
  session** navigation.
- Pinning adds one card to the shared staging inbox. Re-pinning a placed card
  returns that same durable snapshot to staging instead of creating a copy.
  The source project's default tab is created as a suggested destination;
  sessions without a project use the global **Board** tab.

## Tabs and staging

Inner tabs are separate boards, not saved cameras over one shared canvas.
Placed cards belong to exactly one tab, while staged cards remain visible above
every tab until they are placed. Project-default tabs are created on demand,
while custom tabs can mix material from several projects. Custom tabs without
placed cards can be removed, and custom tabs can be renamed at any time. When
a project is deleted, its project-default tab and cards are preserved and the
tab becomes a normal custom tab using its last project name.

A card keeps one immutable snapshot while moving between explicit `staged` and
`placed` states. Staged cards appear as compact chips in one horizontally
scrollable tray above the selected board. Selecting a chip opens its preview;
placing or dragging it assigns the currently selected tab and changes only its
state and geometry. A placed card can return to the shared tray or move to
another tab without recapturing its source message. A separate
`hasCanvasPosition` flag distinguishes never-placed cards from cards returning
to their preserved geometry; workflow state is never inferred from `x`/`y`.

## Layout and persistence

The board fills the available pane and grows only to the extents of its cards.
It has no browser scrollbars: dragging empty canvas pans one transform-backed
content plane. During that gesture selection is suppressed, while ordinary
drag-selection inside a card body remains available for copying response text.

Fn-wheel and Ctrl-wheel/trackpad pinch zoom around the cursor. Fn-wheel depends
on the browser exposing the Fn modifier; Ctrl-wheel remains the portable
fallback. Cmd/Ctrl `+`, `-`, and `0` provide keyboard zoom and reset, with
matching toolbar controls. The center zoom control fits the current board, and
opening a tab repairs a persisted camera only when every placed card would be
off-screen. Zoom is clamped to 25–200% and stored per inner tab
on each outer workspace tab; pan and scale are composed on the content plane. Card
positions and dimensions stay in logical coordinates, so moving, resizing,
dropping, and persisted API geometry remain zoom-independent. Position and
dimensions, placement, and tab membership are persisted in `termal.sqlite`.
The workspace layout persists the active inner tab and each tab's camera, so
two board panes remain independent.

The durable collection remains global and does not wake sessions. Tabs provide
project organization without creating separate board databases. Card snapshots
are capped at 1 MiB; the shared staging inbox and each placed-card canvas are
independently capped at 256 cards. Creating a custom tab is allowed while fewer
than 64 custom tabs exist; project-default tabs are derived from projects and
do not consume that creation limit. Project deletion always preserves and
detaches its tab even when 64 custom tabs already exist, because retaining the
board data takes precedence over the creation cap. The detached tab then counts
as custom for subsequent create requests.

## HTTP API

| Method | Route | Purpose |
| --- | --- | --- |
| GET | `/api/response-board` | Legacy view: placed cards in the default tab. |
| GET/POST | `/api/response-board/tabs` | List tabs or create a custom tab. |
| POST | `/api/response-board/tabs/reorder` | Persist the complete tab order. |
| GET/PATCH/DELETE | `/api/response-board/tabs/{id}` | Read a tab's placed cards plus the shared staged cards, rename it, or delete a custom tab without placed cards. |
| POST | `/api/response-board/cards/stage` | Idempotently stage `{ sessionId, messageId, tabId? | projectId? }`, or atomically place it when `placement: "placed"` and finite `x`/`y` are supplied. |
| POST | `/api/response-board/cards` | Legacy create in the default tab from `{ sessionId, messageId, x, y }`. |
| PATCH | `/api/response-board/cards/{id}` | Update supplied geometry, `placement`, or `tabId`. |
| DELETE | `/api/response-board/cards/{id}` | Remove a card; returns `204`. |

The staging route returns `201` when it creates a new durable card and `200`
when it reuses an existing source card. Reusing a placed card moves that same
card back to the shared staging inbox and applies the supplied destination
hint, so it disappears from its previous canvas. Supplying `placement:
"placed"` with `x` and `y` performs the source lookup, snapshot creation or
reuse, destination-capacity check, and canvas placement in one transaction;
transcript drops use this form and therefore cannot leave a half-applied staged
card. Moving the source to another tab reuses the card rather than copying it.
The route returns `409` when the destination already contains another placed
card for the same source, when the destination canvas has reached its 256-card
limit, or when a staging action would exceed the 256-card global inbox limit.

The create route returns `404` when the requested source message is not yet in
durable history. Geometry is finite and bounded: coordinates are limited to
the board range, width to 240–1600 px, and height to 160–1600 px.

Implementation anchors: `src/response_board.rs`,
`ui/src/panels/ResponseBoardPanel.tsx`, and `ui/src/response-board.ts`.
