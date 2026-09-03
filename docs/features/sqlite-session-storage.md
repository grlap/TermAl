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
- Do not require row-by-row manual migration or file copying for a supported
  current schema. Unsupported unreleased local schemas may require moving the
  named database aside or deleting it to reset that local state.
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

## Current Normalized Schema

The primary state database keeps application and record metadata as JSON while
normalizing the independently loaded session, transcript, prompt-history,
delegation, overview, and response-board projections. The production DDL is:

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
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE TABLE session_overviews (
  session_id TEXT PRIMARY KEY,
  value_blob BLOB NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE TABLE session_prompt_histories (
  session_id TEXT PRIMARY KEY,
  value_json TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE TABLE delegations (
  id TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE response_board_tabs (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  project_id TEXT UNIQUE,
  sort_order INTEGER NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE board_cards (
  id TEXT PRIMARY KEY,
  x REAL NOT NULL,
  y REAL NOT NULL,
  w REAL NOT NULL,
  h REAL NOT NULL,
  snapshot_json TEXT NOT NULL,
  source_session_id TEXT NOT NULL,
  source_message_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  tab_id TEXT NOT NULL DEFAULT 'response-board-default',
  placement TEXT NOT NULL DEFAULT 'placed',
  has_canvas_position INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX board_cards_tab_placement_created_idx
  ON board_cards(tab_id, placement, created_at, id);
```

`app_state.metadataState` stores typed global app metadata without session,
delegation, or message rows. It may be absent only in a freshly initialized,
never-persisted database whose normalized authority tables are empty.
`sessions.value_json` stores one serialized
metadata record with an empty `messages` array. `messages.value_json` stores one
message payload. `overview_kind` and `is_user` are compact semantic fields
maintained with each message write. The composite primary key answers ordered
range reads without a table scan; the unique key answers stable message-id
cursor lookups. `session_overviews.value_blob` stores those semantic fields in
one byte per global message position. It is updated in the same transaction as
the message rows and lets the whole-conversation overview endpoint read one
small row instead of stepping through or parsing every persisted message.
`delegations.value_json` stores one serialized delegation record per row.

Startup validates every normalized row against its durable key: session and
delegation table ids must match the ids embedded in their JSON payloads, and a
message row's `message_id` must match `Message::id()`. Invalid rows are isolated
from the healthy in-memory model and logged. Their ids remain in a runtime
quarantine set so a later synchronous full-snapshot persistence fallback does
not misinterpret the isolated row as a user deletion; the original SQLite row
stays available for recovery or inspection.

`meta.schema_version` is `2`, and
`meta.prompt_history_storage_version` is `1`. Existing databases must contain
all nine tables above, but may retain unrelated tables from earlier development
builds. Each required table must have exactly the column set shown above,
except that the retained current-schema maintenance may add the two message
overview columns and the three board-card partition columns when they are
missing. Unexpected columns and every other missing column are rejected before
maintenance; after maintenance all nine tables must match the canonical sets.
Before any schema maintenance or persistent PRAGMA runs, startup also requires
both metadata markers. A present `app_state.metadataState` row must deserialize
as the current `PersistedState` shape. An absent row is accepted only when
`sessions`, `messages`, `session_overviews`, `session_prompt_histories`,
`delegations`, and `board_cards` contain no rows; the schema-seeded default
response-board tab does not count as persisted app metadata. Startup rejects
the obsolete v2 authority shapes: non-empty `sessions` or `delegations` arrays
in `app_state` (and fail-closed non-null values of the wrong type), or an array-valued
`session.promptHistory` key in a parseable `sessions.value_json` row, including
an empty array. A non-array prompt-history value is damaged row data rather
than legacy authority and remains a row-level quarantine concern, as does
malformed session JSON. Fresh databases create every table and both markers in
one immediate transaction. Any unsupported or partial shape is rejected with
move/delete reset guidance; there is no migration, dual-read, or alternate
hydration path for unreleased local state.

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

1. Open `termal.sqlite` when it exists and validate the required current table
   subset, canonical column sets, schema and prompt-history authority markers,
   typed global metadata authority, and absence of embedded legacy records
   before persistent PRAGMAs, maintenance, or state loading. Only the five
   columns added by retained
   overview/board maintenance may initially be absent, and the post-maintenance
   shape must be canonical.
   A database with no user tables receives all current tables and both markers
   in one immediate transaction.
2. Bootstrap `coordination.sqlite` before any coordination stores, background
   persistence worker, or HTTP listener. An empty file receives the complete
   current schema atomically after emptiness is rechecked under an SQLite
   immediate transaction. An existing file must already match the current
   schema version and canonical schema definitions, including column types and
   constraints, foreign keys, and named indexes. Only a genuinely absent or
   unsupported version/schema receives reset guidance; lock, corruption, I/O,
   and other SQLite read failures remain operational errors naming the actual
   path.
3. Open the long-lived mailbox and board connections and start a dedicated
   coordination-cleanup worker alongside the primary-state persist worker.
4. Queue the boot-state persistence tick. Once it confirms any durable
   project-deletion outbox in `termal.sqlite`, it signals the cleanup worker;
   completed cleanup wakes primary persistence again to clear the outbox.
   Failed or interrupted cleanup remains durable for retry. Because the
   deleted project cannot authorize new board traffic, the HTTP listener can
   open without waiting for a large cascade or a backlog of scopes.

TermAl does not attach `termal.sqlite` or import coordination rows from the
former unreleased single-database layout. If validation reports an unsupported
`termal.sqlite` or `coordination.sqlite`, stop TermAl and move the named file
aside for diagnosis or delete it to reset its local state, then restart.
Current-schema coordination files are never rewritten merely to satisfy startup
validation.

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
- The current v2 schema stores transcript messages in indexed `messages` rows.
  Version 1 databases are rejected with `termal.sqlite` reset guidance rather
  than migrated or read through a compatibility path.
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
retained suffixes, not all historical messages. Runtime reads only the current
indexed tables; obsolete development schemas must be reset instead of migrated.
