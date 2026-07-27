# SQLite Session Storage Plan

> The persisted `app_state` blob also holds the Codex `ignoredDiscoveredCodexThreadIds`
> ignore set (the serialized camelCase key; not exposed via `/api/state`); see
> [shared-codex-app-server.md](./shared-codex-app-server.md).

TermAl persists application/session state in `~/.termal/termal.sqlite` and
durable agent coordination in `~/.termal/coordination.sqlite`. Earlier builds
used one large JSON document, but the current code no longer carries a
`sessions.json` import path.

The target design is SQLite-backed storage with lightweight app state snapshots
and lazy session/message loading.

## Goals

- Session creation should not scale with total historical message count.
- `/api/state` should be fast enough for startup, SSE reconnects, and ordinary
  state adoption.
- Runtime behavior should remain local-only, with no database server and no
  cloud dependency.

## Non-Goals

- Do not fully relationalize every message subtype in the first pass.
- Do not add a complex migration framework before it is needed.
- Do not require users to manually migrate or copy files.
- Do not change agent protocols as part of this work.

## Storage Layout

Use two SQLite writer domains under the TermAl data directory:

```text
~/.termal/
  termal.sqlite       # app metadata, projects, sessions, delegations
  coordination.sqlite # durable mailboxes and coordination boards
```

The split keeps mailbox and board deadlines independent from large transcript
serialization and session-state writes. Mailboxes and boards share the small
coordination database and its FIFO writer admission because their discovery,
lifecycles, and bounded file-descriptor budget remain one domain. See
[durable agent mailboxes](agent-mailboxes.md) and the
[coordination board](agent-boards.md) for their surface contracts.

## Restartable Slice Schema

The first implementation slice keeps the live object model intact and moves the
durable container from one JSON file to SQLite rows:

```sql
CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE app_state (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE delegations (
  id TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);
```

`app_state.metadataState` stores global app metadata without session or
delegation rows. `sessions.value_json` stores one serialized session record per
row, and `delegations.value_json` stores one serialized delegation record per
row. This is not the final lazy-message schema, but it is enough to stop
create/fork persistence from rewriting every historical session in one
monolithic file.

## Target Lazy-Loading Schema

Keep message payloads as JSON so the first migration is mostly a storage and API
boundary change, not a rewrite of the message model.

```sql
CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE app_state (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE projects (
  id TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE workspace_layouts (
  id TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE orchestrators (
  id TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  emoji TEXT NOT NULL,
  agent TEXT NOT NULL,
  status TEXT NOT NULL,
  preview TEXT NOT NULL,
  workdir TEXT NOT NULL,
  project_id TEXT,
  model TEXT NOT NULL,
  settings_json TEXT NOT NULL,
  external_session_id TEXT,
  agent_commands_revision INTEGER NOT NULL DEFAULT 0,
  codex_thread_state TEXT,
  message_count INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE TABLE messages (
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  id TEXT NOT NULL,
  author TEXT NOT NULL,
  type TEXT NOT NULL,
  timestamp TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (session_id, position),
  UNIQUE (session_id, id),
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE queued_prompts (
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (session_id, position),
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_session_id_position
  ON messages(session_id, position);

CREATE INDEX idx_sessions_project_id
  ON sessions(project_id);

CREATE INDEX idx_sessions_updated_at_ms
  ON sessions(updated_at_ms);
```

`meta.schema_version` is `1`. TermAl does not run compatibility migrations for
older local development schemas; a binary that opens an existing database with a
different schema version refuses to start instead of rewriting data with an
unknown layout.

## API Shape

### Summary State

`GET /api/state` should return global app state and session summaries only.
Summaries include enough data to render lists, tabs, project grouping, status,
preview, and settings controls, but not full message arrays.

### Full Session

Add:

```text
GET /api/sessions/{id}
```

This returns the full session metadata plus the most recent message window.

Add later, or in the same pass if cheap:

```text
GET /api/sessions/{id}/messages?before=<position>&limit=200
```

This supports "load earlier messages" without loading the entire conversation.

### Create Session

`POST /api/sessions` should return the created session summary and enough full
session detail to open the new tab immediately. It should not return all
historical sessions and messages.

### SSE

State SSE should stay summary-oriented. Message-heavy changes should be deltas:

- `SessionCreated`
- `SessionSummaryUpdated`
- `MessageAdded`
- `MessageUpdated`
- `MessagesCompacted`

Loaded sessions apply message deltas. Unloaded sessions update only summary
state and preview.

## Startup Flow

On startup:

1. Open `termal.sqlite` when it exists and load metadata, session rows, and
   delegation rows, or bootstrap an empty local state when it has no app
   metadata yet.
2. Bootstrap `coordination.sqlite` before any coordination stores, background
   persistence worker, or HTTP listener.
3. On first split-database boot, attach `termal.sqlite` read-only and copy the
   legacy mailbox/board rows. Copy, invariant verification, and the
   destination-owned migration marker commit atomically.
4. Open the long-lived mailbox and board connections and start a dedicated
   coordination-cleanup worker alongside the primary-state persist worker.
5. Queue the boot-state persistence tick. Once it confirms any durable
   project-deletion outbox in `termal.sqlite`, it signals the cleanup worker;
   completed cleanup wakes primary persistence again to clear the outbox.
   Failed or interrupted cleanup remains durable for retry. Because the
   deleted project cannot authorize new board traffic, the HTTP listener can
   open without waiting for a large cascade or a backlog of scopes.

That first-boot attachment assumes one TermAl process owns the data directory.
Do not start a second instance against the same `~/.termal` (even on another
port) while migration is pending: its read-only attachment would still open
the first process's live WAL database. Stop the existing process before
repairing or retrying a marker-absent migration.

If legacy verification fails, the destination copy and migration marker roll
back together and TermAl refuses to start; the HTTP listener has not opened,
so no live coordination traffic can race recovery. Stop TermAl and preserve
both database files. Repair or restore the legacy `termal.sqlite`, or install
a build that understands the reported legacy schema, then restart: the absent
marker makes the import retry from the beginning. Do not insert the marker by
hand. A marker-absent destination must contain no mailbox, board, or deleted-
scope rows: TermAl refuses to merge an independently populated destination
because same-key payload conflicts cannot be resolved safely. If
`coordination.sqlite` was created solely by that failed first boot and it is
certain no earlier split-database build ever served traffic from it, moving
that unopened destination aside is safe and lets TermAl create a fresh
destination; otherwise retain it for diagnosis rather than discarding durable
mailbox or board data.

## Frontend Changes

Split frontend state into:

- Session summaries from `/api/state`.
- Loaded session details keyed by session id.

Session tabs render immediately from summaries. Opening a session tab requests
the full session if it is not loaded yet.

Creating a session should:

1. Call `POST /api/sessions`.
2. Add/open the returned session immediately.
3. Close the create dialog immediately.
4. Refresh model options in the background.
5. Show model refresh failures as session-level notices, not failed creates.

## Implementation Order

1. Make model refresh after session creation fire-and-forget.
2. Add SQLite dependency and a small storage module.
3. Add schema creation and JSON import with post-import rename.
4. Change create/fork session responses to return the created session directly
   and publish a `sessionCreated` delta instead of a full historical state
   snapshot.
5. Persist newly created sessions with a metadata update plus one session row
   instead of cloning every historical message.
6. Add session summary/full-session API types while keeping old endpoints
   temporarily compatible.
7. Update frontend to use summaries plus lazy full-session loading.
8. Move remaining mutation persistence writes from full-state snapshots to
   targeted SQLite row updates.
9. Remove full messages from `/api/state`.
10. Update SSE to avoid full message snapshots for ordinary non-create changes.
11. Delete temporary compatibility code once tests cover the new flow.

## Current Implementation Status

The first restartable slice is implemented:

- Production startup stores state in `~/.termal/termal.sqlite`.
- SQLite stores global metadata separately from per-session JSON rows.
- Creating or forking a session persists only global counters plus the created
  session row.
- Create/fork responses return the created session directly and publish a small
  `sessionCreated` delta.
- `GET /api/sessions/{id}` returns one authoritative full session plus the
  current revision.
- The frontend has a small on-demand hydration path for future summary sessions
  that explicitly arrive with `messagesLoaded: false`.

The remaining performance work is the broader summary/lazy-loading API split and
targeted row updates for non-create mutations.

## Test Plan

Backend tests:

- Exercise SQLite startup load/save directly under `cargo test`.
- `GET /api/state` excludes full message arrays.
- `GET /api/sessions/{id}` returns full session details.
- Creating a session inserts one session row and does not load/write unrelated
  messages.
- Appending/updating a message touches only that session's message rows.

Frontend tests:

- Create session opens the tab before model refresh resolves.
- Opening an unloaded session fetches full details.
- Summary SSE updates do not discard loaded messages.
- Message deltas update only the loaded target session.
- Long-history fixtures do not force full message reconciliation on create.

## Expected Result

Session creation becomes proportional to the new session plus small summary
state, not proportional to all historical messages. Existing users keep their
data through automatic import, and the renamed JSON file remains a simple local
backup.
