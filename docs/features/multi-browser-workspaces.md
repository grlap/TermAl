# Feature Brief: Multi-Browser Workspaces

## Status

Implemented.

Browser tabs share live updates through the [shared live event transport](./shared-live-events.md).
Layout autosave retries transient network/HTTP failures with backoff. Permanent
HTTP rejection stops automatic retries and displays an error; the layout remains
in browser storage with a pending-save identity. A subsequent layout edit tries
again. On reload, an unsaved local layout takes precedence over the server copy
and is submitted again; only a matching successful save clears its pending mark.

## Problem

Browser-local layout storage is not enough for a control room that may run in
several browser windows or on several monitors. Each window needs an independent
layout when desired, and an intentional shared layout when the same URL is
opened elsewhere.

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

A JSON `404` for the workspace layout keeps the local fallback and enables its
first server save. Autosave watches the serialized layout content: session
updates that rebuild an equivalent pane tree neither reset the save delay nor
send another layout write. Actual layout and preference edits still autosave.
Transient failures retain the latest local layout and retry with backoff, starting
at one second and capped at 30 seconds. A newer edit replaces the pending retry.
Permanent rejections and malformed JSON stop automatic retries and show an error.
An interrupted response body is a transport failure, not malformed JSON; a known
permanent HTTP status still stops retries. The layout and pending identity are
stored atomically before sending, so an interrupted or rejected save survives
reload without an older server layout replacing it. This is local recovery, not
a multi-writer merge protocol; intentional same-workspace writers remain last-write-wins.

If reading the pending recovery state during hydration fails, workspace-tree
adoption and autosave pause with a dedicated, persistent storage notice. Server
tree adoption publishes its appearance preferences only after the browser write
succeeds, including initial and deferred hydration. An unreadable marker is unknown, not proof that no
unsaved work exists: the failed hydration must not overwrite its storage entry.
After restoring storage access, choose **Retry workspace save** without reloading.
Retry checks storage again and preserves edits made while paused as a local
pending save before re-fetching. The same check runs before immediate or deferred
adoption, so edits made while the retry is waiting are protected too. Retry does
not classify locally retained delegated-child tabs or canvas cards as restored tabs.
References actually adopted from the server during retry do enter the restore
scope, including when parent-delegation metadata arrives later. Server preferences
are deferred with the tree, and a storage write must succeed before either is
published. A pending local layout still wins as a whole; this does not introduce
a preference merge into pending local work. The notice remains during recovery
and is not cleared by unrelated actions. Another read/write failure or a failed
retry GET keeps saving paused and enables another Retry; restored browser storage
alone never authorizes saving the older bootstrap tree. Once a workspace has
hydrated, a storage pause or retry does not undo
that readiness: Git-diff document restoration and restore-only child pruning
continue, while autosave and pagehide flushing remain paused. Initial hydration
that has not completed stays unready. A retry has a 15-second deadline covering both its fetch and any wait for
session metadata: expiry cancels that attempt, ignores late results, and enables
Retry again without claiming that anything was saved. Unmount cancels the attempt.
This does not change conflict precedence or add blocked-storage cold-start support.

### Reopen existing workspace

If the URL already contains `?workspace=review-monitor`, the frontend loads and
persists the layout under that ID.

This supports:

- one browser on the left monitor with `?workspace=planner`
- one browser on the right monitor with `?workspace=review`
- an intentionally shared layout by opening the same URL in another browser

In the Workspace switcher, choose **Open in new tab** beside another saved
workspace to open its existing layout while keeping this tab in place. This
is a native link: middle-click, Ctrl/Cmd-click, and the browser's link context
menu work too. Clicking the main row still switches this tab. No workspace
is copied or created by the new-tab action.

The current row has no new-tab action: two tabs saving the same workspace ID
would overwrite the same saved layout. This does not detect whether another
saved workspace is already open elsewhere; same-ID writes remain last-write-wins.

### Workspace labels

Open **Workspace → Add current label** (or **Edit current label**) to name
the workspace in the current browser tab. The editor stays above the saved
list and only edits the current workspace. Labels appear in the switcher,
its current-workspace button, and the browser tab title. Workspace IDs and
URLs stay unchanged; an empty label restores the ID-based display.

The saved list pins the current workspace first, then sorts by label or ID,
with the ID as a tie-breaker. Live updates and layout autosaves do not reorder
the list by activity time.

Labels are optional server-persisted metadata, limited to 80 characters.
`PATCH /api/workspaces/{id}/label` accepts `{ "label": "Reviews" }` and returns
the saved layout document. The separate endpoint changes only the label:
ordinary layout PUTs preserve it even when sent by an older browser tab.
Workspace GET, list, and state-event summaries expose the label when set.
Existing saved workspaces without labels remain valid.

### Local cache

The browser still keeps a per-workspace local cache as a warm-start fallback,
but the server is the source of truth. Same-tab workspace switches should flush
any pending debounced save before navigation so the backend copy stays current.

Only `termal-workspace-layout:<workspace-id>` is read for a browser layout.
Old unscoped localStorage keys are ignored, never read or migrated. A browser
holding only old keys starts from defaults until a valid current server layout
is loaded. A current workspace has explicit nullable `lastContentPaneId` and
`lastViewerPaneId` routing fields alongside `root`, `panes`, and `activePaneId`;
documents missing either routing field are ignored rather than upgraded.
Current stale-pane routing recovery and per-pane scroll-state moves are separate
runtime behaviors and are unchanged.

Theme persistence uses only `lightThemeId`, `darkThemeId`, and `themeMode`,
with global browser slots `termal-ui-theme-light`, `termal-ui-theme-dark`, and
`termal-ui-theme-mode`. The retired `themeId` field is absent from the server's
document, summary and PUT types as well as the client contract. It is never
stored or returned; an unknown request field does not acquire theme authority.
The browser never reads or writes the retired `termal-ui-theme` key.
Current workspace theme fields take precedence over current global slots,
then built-in defaults.
See [Configurable UI Themes](../themes.md) for the current preference keys.
Unknown cosmetic `diagramLook` values are dropped without changing the pane/tab
arrangement; the current stored preference or default supplies the look. There
is no mapping from a retired look to another look.

## Data model

Workspace views live in the main persisted backend state alongside projects,
sessions, and orchestration instances.

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

The backend treats the nested `workspace` payload as an opaque JSON document.
The frontend remains responsible for schema validation.

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
    "lightThemeId": "warm-light",
    "darkThemeId": "dark",
    "themeMode": "auto",
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
  "lightThemeId": "warm-light",
  "darkThemeId": "dark",
  "themeMode": "auto",
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

Workspace persistence is debounced slightly so split dragging and canvas
dragging do not write on every pointer move. Pending saves are flushed with
`keepalive` on pagehide/unload paths where possible.

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
