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

## Current Schema

Schema v2 keeps session metadata JSON-shaped while moving ordered transcript
messages into their own indexed rows:

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

CREATE TABLE messages (
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL CHECK(position >= 0),
  message_id TEXT NOT NULL,
  value_json TEXT NOT NULL,
  overview_kind INTEGER NOT NULL DEFAULT 0,
  is_user INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(session_id, position),
  UNIQUE(session_id, message_id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE TABLE session_overviews (
  session_id TEXT PRIMARY KEY,
  value_blob BLOB NOT NULL,
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE TABLE delegations (
  id TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);
```

`app_state.metadataState` stores global app metadata without session,
delegation, or message rows. `sessions.value_json` stores one serialized
metadata record with an empty `messages` array. `messages.value_json` stores one
message payload. `overview_kind` and `is_user` are compact semantic fields
maintained with each message write. The composite primary key answers ordered
range reads without a table scan; the unique key answers stable message-id
cursor lookups.
`session_overviews.value_blob` stores those semantic fields in one byte per
global message position. It is updated in the same transaction as the message
rows and lets the whole-conversation overview endpoint read one small row
instead of stepping through or parsing every persisted message.
`delegations.value_json` stores one serialized delegation record per row.

Startup validates every normalized row against its durable key: session and
delegation table ids must match the ids embedded in their JSON payloads, and a
message row's `message_id` must match `Message::id()`. Invalid rows are isolated
from the healthy in-memory model and logged. Their ids remain in a runtime
quarantine set so a later synchronous full-snapshot persistence fallback does
not misinterpret the isolated row as a user deletion; the original SQLite row
stays available for recovery or inspection.

## Longer-Term Fully Columnar Schema

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

`meta.schema_version` is `2`. The single supported v1-to-v2 upgrade moves each
embedded session message array into indexed `messages` rows in one transaction,
then records schema v2. Any other schema version is rejected; there is no runtime
dual-read or alternate hydration path.

## API Shape

### Summary State

`GET /api/state` should return global app state and session summaries only.
Summaries include enough data to render lists, tabs, project grouping, status,
preview, and settings controls, but not full message arrays.

### Bounded Session Detail

Add:

```text
GET /api/sessions/{id}
```

This returns session metadata plus the most recent 20-message window. The
optional `tail` limit is capped at 64; there is no unbounded transcript form.

```text
GET /api/sessions/{id}/history?before=<message-id>&limit=64
GET /api/sessions/{id}/history?from=start&limit=64
GET /api/sessions/{id}/history?after=<message-id>&limit=64
```

These load one ascending bounded page without serializing the entire
conversation. `before` and `after` are stable exclusive message-id cursors;
`from=start` returns the true first page. The modes are mutually exclusive.

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
- Bounded session windows keyed by session id.

Session tabs render immediately from summaries. Opening a session tab requests
the recent tail if it is not loaded yet; older pages are demand-loaded.

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
6. Add session summary and bounded-session API types.
7. Update frontend to use summaries plus demand-loaded transcript pages.
8. Move remaining mutation persistence writes from full-state snapshots to
   targeted SQLite row updates.
9. Remove full messages from `/api/state`.
10. Update SSE to avoid full message snapshots for ordinary non-create changes.
11. Add bidirectional page cursors and bounded browser-window eviction.

## Current Implementation Status

The normalized transcript schema and bounded HTTP reads are implemented:

- Production startup stores state in `~/.termal/termal.sqlite`.
- SQLite stores global metadata and per-session metadata separately from
  transcript messages.
- Schema v2 migrates message arrays out of v1 `sessions.value_json` rows in one
  transaction without losing order or message ids. It reads legacy sessions in
  bounded batches and isolates malformed rows so one damaged session cannot
  prevent healthy siblings from migrating.
- `messages` is a `WITHOUT ROWID` table with
  `PRIMARY KEY(session_id, position)` for ordered range pages and
  `UNIQUE(session_id, message_id)` for stable cursor resolution.
- Creating or forking a session persists only global counters plus the created
  session row.
- Create/fork responses return the created session directly and publish a small
  `sessionCreated` delta.
- `GET /api/sessions/{id}` returns a recent 20-message suffix by default and
  never returns an unbounded local transcript.
- `/history` returns at most 64 messages per request in backwards,
  true-start, forwards, or centered `around` mode.
- `/overview` returns one whole-conversation, position-linear semantic map from
  the compact per-session overview blob plus the retained live tail. The same
  session produces the same map regardless of transcript residency.
- The frontend demand-loads older pages and does not schedule a background
  all-history fetch.
- Startup loads at most the latest 64 messages per session. After a successful
  persistence pass, idle in-memory session records are trimmed to the same
  64-message suffix; older pages are read from SQLite only on demand.
- Startup treats each normalized session/delegation row as an isolation
  boundary: malformed metadata, invalid embedded ids/settings, or unreadable
  transcript rows are reported and skipped without making every other session
  unreachable. A known transcript count with no local rows is an unhydrated
  proxy/window state rather than a process-fatal error.
- Remote proxy tails and history pages remain bounded across the remote
  boundary rather than materializing a complete owner transcript locally.

The remaining memory-bound work is browser-side bidirectional page eviction.
The browser does not automatically fetch all history, but pages a user has
explicitly visited remain resident for that browser session.

## Test Plan

Backend tests:

- Exercise SQLite startup load/save directly under `cargo test`.
- `GET /api/state` excludes full message arrays.
- `GET /api/sessions/{id}` always returns the default 20-message suffix unless
  a bounded `tail` is requested.
- `GET /api/sessions/{id}/history` returns an ascending page using one of
  `before={messageId}`, `after={messageId}`, `around={position}`, or
  `from=start`, with `N <= 64`.
- `GET /api/sessions/{id}/overview?buckets=512` remains under 8 KiB on the
  compressed wire and answers a 25k-message persisted transcript in under
  10 ms.
- Creating a session inserts one session row and does not load/write unrelated
  messages.
- Appending/updating a message touches only that session's message rows.

Frontend tests:

- Create session opens the tab before model refresh resolves.
- Opening an unloaded large session fetches its recent tail first, then prepends
  bounded history pages on scroll or explicit marker-navigation demand.
- Resident-only search labels partial results instead of claiming it searched
  history that is not loaded.
- Summary SSE updates do not discard loaded messages.
- Message deltas update only the loaded target session.
- Long-history fixtures do not force full message reconciliation on create.

## Expected Result

Session creation and startup are proportional to session metadata plus bounded
retained suffixes, not all historical messages. The one-time v1-to-v2 SQLite
migration preserves existing transcript order and ids; normal runtime reads only
the v2 indexed tables.
