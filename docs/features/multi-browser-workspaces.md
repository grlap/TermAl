# Feature Brief: Multi-Browser Workspaces

Backlog source: proposed feature brief; not yet linked from `docs/bugs.md`.

## Problem

This brief originally described workspace layout as browser-local only. The
current implementation is more capable: each workspace view is cached per
workspace in browser `localStorage` and persisted on the backend, and the UI
now includes a workspace switcher backed by the server.

The remaining problem is documentation clarity. Readers need the actual
multi-browser model and API surface that shipped, including the server list
route and switcher-driven flow.

## Core idea

Introduce server-backed **workspace views**.

A workspace view is a persisted layout document:

- split tree
- open tabs
- active pane/tab
- control-panel dock side
- canvas tab card positions and zoom

Each browser window opens one workspace view at a time. The view identity lives
in the URL as `?workspace=<id>`.

That gives TermAl two important behaviors:

1. Different browser windows can persist different layouts against the same
   running server.
2. Copying the exact URL into another browser opens the same workspace view on
   purpose.

This is not collaborative layout editing. In Phase 1, a workspace view is
single-writer in practice, and if two browsers open the same view at once,
last write wins.

## User experience

### Default open

When the user opens TermAl without a `workspace` query parameter:

- the frontend generates a new workspace view ID
- rewrites the URL with `?workspace=<generated-id>`
- loads that workspace view from the server if it already exists
- otherwise starts from the local fallback/default layout

That means a fresh browser window naturally gets its own layout instead of
fighting over a shared browser-global key.

### Reopen existing workspace

If the URL already contains `?workspace=review-monitor`, the frontend loads and
persists the layout under that ID.

This supports:

- one browser on the left monitor with `?workspace=planner`
- one browser on the right monitor with `?workspace=review`
- an intentionally shared layout by opening the same URL in another browser

### Local cache

The browser still keeps a per-workspace local cache as a warm-start fallback,
but the server is the source of truth. Same-tab workspace switches should flush
any pending debounced save before navigation so the backend copy stays current.

## Data model

Workspace views should live in the main persisted backend state alongside
projects, sessions, and orchestration instances.

```rust
struct WorkspaceLayoutDocument {
    id: String,
    revision: u64,
    updated_at: String,
    control_panel_side: WorkspaceControlPanelSide,
    workspace: serde_json::Value,
}

enum WorkspaceControlPanelSide {
    Left,
    Right,
}
```

And in the persisted state:

```rust
struct StateInner {
    // existing fields...
    workspace_layouts: BTreeMap<String, WorkspaceLayoutDocument>,
}
```

The backend treats the nested `workspace` payload as an opaque JSON document in
Phase 1. The frontend remains responsible for schema validation.

## API

The shipped Phase 1 API includes a list route for the workspace switcher in
addition to direct get/put by ID.

```text
GET /api/workspaces
GET /api/workspaces/{id}
PUT /api/workspaces/{id}
DELETE /api/workspaces/{id}
```

### GET `/api/workspaces/{id}`

Returns:

```json
{
  "layout": {
    "id": "planner-monitor",
    "revision": 3,
    "updatedAt": "2026-03-28 10:24:11",
    "controlPanelSide": "left",
    "workspace": { "...": "workspace document" }
  }
}
```

If the workspace view does not exist, return `404`.

### GET `/api/workspaces`

Returns a summary list ordered by most recent update. The frontend uses this to
populate the workspace switcher and to reopen saved browser layouts.

### PUT `/api/workspaces/{id}`

Request:

```json
{
  "controlPanelSide": "left",
  "workspace": { "...": "workspace document" }
}
```

Behavior:

- create the workspace view if it does not exist
- replace the stored layout document
- increment `revision`
- bump the global app `revision`
- emit a fresh `/api/state` snapshot so other connected switchers see the updated summaries

## Concurrency semantics

Phase 1 intentionally keeps the rule simple:

- different workspace IDs are independent
- same workspace ID in multiple browsers is allowed
- if multiple browsers write the same workspace ID, last write wins

This is acceptable because the main use case is one workspace view per monitor.

Future improvements can add optimistic concurrency or live layout events, but
that should not block the first useful version.

## Frontend behavior

### Bootstrap

The frontend boot order becomes:

1. resolve or generate the workspace view ID from the URL
2. read the per-workspace local cache for a fast initial paint
3. fetch the server-backed workspace view
4. if the server has a valid layout, adopt it
5. begin persisting local changes back to the server

### Persistence

Workspace persistence should be debounced slightly so split dragging and canvas
dragging do not write on every pointer move.

### Validation

The existing frontend workspace validation remains the gatekeeper:

- malformed local cache is ignored
- malformed server payload is ignored
- invalid tabs/panes are reconciled against the current session list as today

## Non-goals for Phase 1

- collaborative live layout editing
- visual presence indicators showing which browser owns which workspace
- rename management beyond the current switcher list
- server-side semantic understanding of every workspace tab variant

Those can come later once the basic multi-browser workflow is solid.

## Implementation plan

1. Add `workspace_layouts` to the persisted backend state.
2. Add `GET /api/workspaces/{id}`, `PUT /api/workspaces/{id}`, and `DELETE /api/workspaces/{id}`.
3. Generate or read `?workspace=<id>` in the frontend.
4. Move workspace persistence from one global `localStorage` key to:
   - per-workspace local cache
   - server-backed list/get/put routes
5. Add a workspace switcher that can list saved layouts, open another workspace
   in the current tab, or spawn a new browser window with a fresh workspace ID.
6. Keep the current layout model and reconciliation logic unchanged.

## Acceptance criteria

- Opening TermAl in two separate browser windows from the bare root URL results
  in two different workspace IDs and two independent persisted layouts.
- Reloading either window restores that window's layout.
- Opening the exact same `?workspace=<id>` URL in another browser restores the
  same workspace view.
- Layout persistence no longer depends on one browser-global localStorage key.
- Workspace layout saves publish updated `/api/state` snapshots so other browser switchers
  stay in sync.
