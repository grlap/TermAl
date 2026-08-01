# Response Board

The Response Board is one durable, human-facing spatial workspace for pinning
agent responses. It is separate from the agent [Coordination
Board](agent-boards.md): the coordination board stores small versioned JSON
facts for agents, while the Response Board renders immutable transcript-card
snapshots for a person.

## Opening and pinning

- **Open Response Board** in the left control surface opens or focuses the
  singleton workspace tab.
- A transcript message can be pinned with **Pin to board** or dragged from its
  metadata header. The header is the drag handle so text in the message body
  remains selectable; the drag preview still shows the complete message card.
- Drag payloads carry only `sessionId` and `messageId`. The server resolves the
  durable transcript row and creates the snapshot. Browser-supplied message
  bodies are never accepted as board content.
- Cards retain their content if the source transcript is later pruned. Their
  source ids and durable message position remain available for **Open in
  session** navigation.

## Layout and persistence

The board fills the available pane and grows only to the extents of its cards.
It has no browser scrollbars: dragging empty canvas pans one transform-backed
content plane. During that gesture selection is suppressed, while ordinary
drag-selection inside a card body remains available for copying response text.

Ctrl-wheel/trackpad pinch zooms around the cursor. Cmd/Ctrl `+`, `-`, and `0`
provide keyboard zoom and reset, with matching toolbar controls. Zoom is
clamped to 25–200% and stored as local per-board view state; pan and scale are
composed on the content plane. Card positions and dimensions stay in logical
coordinates, so moving, resizing, dropping, and persisted API geometry remain
zoom-independent. Position and dimensions are persisted in `termal.sqlite`'s
`board_cards` table; view zoom is stored only in browser `localStorage`.

The board is intentionally global and singular in v1. It is not scoped to a
project, session, browser workspace, or agent, and it does not wake sessions.
Card snapshots are capped at 1 MiB and the board at 256 cards.

## HTTP API

| Method | Route | Purpose |
| --- | --- | --- |
| GET | `/api/response-board` | Return all cards. |
| POST | `/api/response-board/cards` | Create a card from `{ sessionId, messageId, x, y }`; returns `201`. |
| PATCH | `/api/response-board/cards/{id}` | Replace `{ x, y, w, h }` after validation. |
| DELETE | `/api/response-board/cards/{id}` | Remove a card; returns `204`. |

The create route returns `404` when the requested source message is not yet in
durable history. Geometry is finite and bounded: coordinates are limited to
the board range, width to 240–1600 px, and height to 160–1600 px.

Implementation anchors: `src/response_board.rs`,
`ui/src/panels/ResponseBoardPanel.tsx`, and `ui/src/response-board.ts`.
