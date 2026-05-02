# SQLite Session Storage Plan

TermAl currently persists application state in one large JSON document at
`~/.termal/sessions.json`. That file includes every visible session and every
message. As conversation history grows, ordinary actions such as creating a new
session can become slow because the backend and browser repeatedly clone,
serialize, parse, and reconcile unrelated historical messages.

The target design is SQLite-backed storage with lightweight app state snapshots
and lazy session/message loading.

## Goals

- Session creation should not scale with total historical message count.
- `/api/state` should be fast enough for startup, SSE reconnects, and ordinary
  state adoption.
- Existing `~/.termal/sessions.json` data must import automatically once.
- The old JSON file should be renamed after successful import so sessions are
  not imported twice.
- Runtime behavior should remain local-only, with no database server and no
  cloud dependency.

## Non-Goals

- Do not fully relationalize every message subtype in the first pass.
- Do not add a complex migration framework before it is needed.
- Do not require users to manually migrate or copy files.
- Do not change agent protocols as part of this work.

## Storage Layout

Use a single SQLite database under the TermAl data directory:

```text
~/.termal/
  termal.sqlite
  sessions.imported-YYYY-MM-DD-HHMMSS.json
```

After a successful first import, rename:

```text
sessions.json -> sessions.imported-YYYY-MM-DD-HHMMSS.json
```

That renamed file is both the backup and the guard against accidental reimport.

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

## Import Flow

On startup:

1. If `termal.sqlite` exists, open it and skip JSON import.
2. If SQLite does not exist and `sessions.json` exists, load the JSON once.
3. Create SQLite schema in a transaction.
4. Insert global state, projects, workspace layouts, orchestrators, sessions,
   messages, and queued prompts.
5. Commit the transaction.
6. Rename `sessions.json` to `sessions.imported-YYYY-MM-DD-HHMMSS.json`.
7. Start TermAl from SQLite state.

If import fails, leave `sessions.json` unchanged and report the startup error.

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
- If `~/.termal/sessions.json` exists and SQLite does not, startup imports it
  once and renames it to `sessions.imported-YYYY-MM-DD-HHMMSS.json`.
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

- Imports an old monolithic `sessions.json` into SQLite.
- Renames the imported JSON file only after a successful transaction.
- Does not reimport when the SQLite database already exists.
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
