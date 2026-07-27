# Agent Coordination Boards

Small, versioned JSON facts scoped to one local project, shared by that
project's root sessions. The board is **level-triggered state** — durable
truth that peers read at their own pace — and is the deliberate complement of
[durable agent mailboxes](agent-mailboxes.md), which carry **edge-triggered
events** that wake sessions. Board writes never wake anyone: if a change
needs someone's attention *now*, send a mailbox message pointing at the key.

Tracker: tm-uwx.7 (design v1.1 on the bead records the negotiated contract
with code citations). Store: `src/coordination_board.rs`. HTTP surface:
`src/board_routes.rs`. MCP tools: `src/delegation_mcp.rs`
(`termal_board_list` / `termal_board_get` / `termal_board_set`).

Board tables live beside mailbox tables in
`~/.termal/coordination.sqlite`. Both stores retain one long-lived connection
and share coordination-local FIFO writer admission, isolated from
`termal.sqlite` session/transcript persistence.

## Model

- **Scope** = one local project (`project.id`). Callers never name the scope;
  it is derived from the calling session's project. Sessions without a
  project, hidden sessions, delegation children (by parent marker OR the
  durable delegation index — independent evidence sources), remote proxies,
  and sessions in remote projects are all rejected by the backend itself; MCP
  tool filtering for delegation children is defense-in-depth on top.
- **Entry** = dotted key → JSON value with a per-key `revision` (starts at 1),
  `updatedAtGeneration` (the scope generation when that key was last written),
  plus author, timestamp, and optional `stateStamp`.
- **Generation** = scope-wide counter bumped once per successful non-duplicate
  write. List responses expose the current value as `generation`; single-key
  reads expose it as `scopeGeneration`, distinct from the key's historical
  `updatedAtGeneration`. Only the current scope value is valid as
  `knownGeneration` or `snapshotGeneration`.
- Keys: 1–8 dotted segments, each starting `[a-z0-9]` then `[a-z0-9_-]*`,
  ≤128 bytes total (e.g. `activity.rust-suite`, `freeze.fingerprint`).
- Values: canonical JSON (sorted object keys), ≤4096 bytes encoded, depth ≤32.
  `null` is a legitimate value, distinct from deletion.

## Write semantics (CAS + idempotency)

Every write carries `expectedRevision` and an author-scoped `idempotencyKey`.

| Intent | Input | Against | Result |
| --- | --- | --- | --- |
| Create | `expectedRevision: 0` + `value` | never-existed key | revision 1 |
| Create | `expectedRevision: 0` + `value` | live head or tombstone | 409 with current head / tombstone revision in `detail` |
| Update | exact live revision + `value` | matching head | revision +1 |
| Restore | exact **tombstone** revision + `value` | tombstoned key | revision +1 (conscious resurrection — the only way back) |
| Delete | exact live revision + `delete: true` (no `value`) | matching head | tombstone, revision +1 |
| Delete | exact tombstone revision | tombstoned key | 404 "already absent" |
| Delete | mismatched revision | tombstoned key | 409 with the tombstone revision in `detail` |
| Delete | any revision | never-created key | 404 |

- `expectedRevision: 0` is strictly create-only. It never resurrects a
  deleted key — that guards against ABA: a stale pre-creation writer cannot
  silently undo a deliberate deletion. Restoration requires reading the
  tombstone's revision from the 409 `detail` and CAS-ing against it.
- Revisions never reset. Tombstones persist (values stripped) so the CAS
  token space is stable forever.
- Idempotency: the lookup namespace is scope + author session +
  `idempotencyKey` (the board key plays no part in the lookup). Replaying the
  identical full request — same key, value, `expectedRevision`, and
  `stateStamp` — returns the original receipt with `duplicate: true`;
  reusing the same `idempotencyKey` for *any* different intent → 409.
  Receipts survive history compaction inside the bounded 4,096-write replay
  window described below.
- On the wire, deletion is `delete: true` with the `value` field **absent** —
  JSON cannot express the difference between "value: null" and "no value", so
  the discriminator is explicit.
- Typed storage-busy rejections distinguish writes ("…no coordination board
  write was committed by this operation…") from reads ("…no mutation was
  attempted by this read operation…"). Both prove exact-request replay is
  safe; the delegation-MCP bridge retries them automatically with bounded,
  session-jittered backoff (tm-uwx.7.4). Repeated 503s after that mean
  sustained contention, not corruption.

## Read semantics

- `get` and `list` never return deleted keys as entries; "never existed" and
  "deleted" are both 404 on read, but a `get` 404 for a *deleted* key may
  carry the tombstone head (revision, `deleted: true`, structurally null
  value) in its `detail` — the same restore token write conflicts expose.
- A successful `get` returns both `updatedAtGeneration` (when that key last
  changed) and `scopeGeneration` (the scope's current counter). These values
  intentionally diverge when another key was written later.
- `list` is sorted by key, default page 100 / max 200, and is
  **generation-bound**: page continuations carry `snapshotGeneration`, and
  any mutation between pages yields 409 — restart the listing. A busy scope
  larger than one page can therefore livelock a listing; accepted for v1
  because expected scale is well under one page (the 512-live-key cap bounds
  the returned set).
- `knownGeneration` on a first page returns `unchanged: true` with zero rows
  when nothing moved — the O(1) turn-start check.

## Limits and lifecycle

- **512 live keys and 4,096 lifetime distinct key names per scope.**
  Tombstones retain their ABA-safe revision tokens but do not consume live
  capacity, so deleting a fact frees a live slot. Restoring a retained
  tombstone reuses its distinct-name slot but still consumes a live slot and
  is rejected while all 512 are occupied. A brand-new name is rejected after
  4,096 distinct heads, so ephemeral key churn cannot grow the scope without
  bound; reuse stable names or delete the project to clear the whole scope.
- History: last 100 revisions per live key (excluding the current head).
  Deleting a key purges its historical values while retaining the single
  tombstone head and revision needed for ABA-safe restoration.
- Idempotency: latest 4,096 successful writes per scope, ordered by durable
  insertion rather than wall-clock timestamps. This bounded replay window
  comfortably covers transport-loss retries without making the receipt table
  grow forever. Within the window, the exact original receipt is replayed.
  Once a receipt ages out, monotonic revisions and tombstones still prevent
  the old request from being silently applied again, but the caller receives
  the current CAS outcome rather than the historical receipt.
- Scope deletion happens **only** through a crash-consistent
  project-deletion outbox. The project removal and pending cleanup first land
  in `termal.sqlite`; only then does a dedicated cleanup worker install an
  idempotent deletion fence and cascade the scope in `coordination.sqlite`.
  Unfinished outbox entries are scheduled again on boot. Any cleanup failure
  stays durably queued; the already-absent project prevents HTTP callers from
  authorizing new work for that scope in the meantime. The fence rejects any
  already-authorized stale write that arrives after cleanup. Lifecycle cleanup
  uses a short coordination-admission budget, and the cascade runs outside
  both boot and the primary persist worker. A busy or large secondary scope
  therefore cannot delay the HTTP listener or primary session persistence.
  There is deliberately no agent-facing scope wipe: it would bypass per-key
  CAS and erase the tombstones and receipts the safety model depends on.
- The coordination-side deletion fence is permanent and grows by one row per
  deleted project. Newly created projects use UUID-backed ids, so a future
  project cannot inherit either the deleted scope's fence or its former
  facts after the primary database is restored or reset.
- The read-only UI polls live while visible: every 8 s it issues the cheap
  `knownGeneration` unchanged probe (zero rows returned when quiet), skips
  ticks while the tab is hidden, and highlights entries whose
  `updatedAtGeneration` moved past the previously rendered generation for a
  few seconds after a change lands. A failed automatic probe keeps the last
  good facts visible, changes the live indicator to a non-assertive `stale`
  state, and retries with bounded exponential backoff; a manual Refresh still
  reports an actionable error immediately. Returning to a visible tab also
  bypasses any outstanding background delay because that focus transition is
  itself a freshness signal. Background multi-page reads use a smaller
  snapshot-conflict restart budget than manual reads so a continuously written
  board cannot consume the full foreground retry cost every poll.
  Completed pages renew the in-flight claim, while a request that makes no
  progress for the stale window is aborted before its replacement starts.
  Consecutive stale-claim replacements receive exponentially longer
  no-progress windows, capped at 128 s, so a slow finite first response gets
  substantially more room without letting a long chain of hung fetches suspend
  recovery indefinitely. Ordinary terminal network errors affect retry
  backoff but do not inflate that stale window, and idle-probe backoff never
  suppresses inspection of an already-active claim's no-progress deadline.
  Truly hung requests are still reclaimed, with decreasing retry pressure.
  Returning to a visible tab replaces a pending background probe as well as
  bypassing backoff. If live
  polling takes over a stalled manual Refresh, the foreground action reports
  that timeout instead of silently degrading to background-only status. The
  header uses the newest surviving fact's exact write time when available; for
  a deletion-only generation, whose deleted row has no timestamp in the list
  response, it labels the local observation time as `last change`. The rendered
  generation is still only the generation observed by the last successful
  probe, and the board never emits SSE wake-ups — polling on the unchanged
  short-circuit is the sanctioned liveness mechanism.
- Newly created projects use collision-resistant `project-<uuid>` ids.
  Existing persisted ids remain valid, but a restored/reset `termal.sqlite`
  cannot reuse the rewindable legacy counter and accidentally inherit a live
  board scope or permanent fence from the independently durable
  `coordination.sqlite`.
- Local-authoritative v1: the board exists only on the local instance; remote
  projects and proxies are rejected and nothing syncs. Any future remote
  story must be explicit, not incidental.

## Relationship to mailboxes

| | Mailbox ([doc](agent-mailboxes.md)) | Board (this doc) |
| --- | --- | --- |
| Trigger model | Edge — messages activate sessions | Level — facts sit still |
| Ordering | Dense per-mailbox sequence, FIFO | Per-key revisions + scope generation |
| Read discipline | Cursor CAS (`processedThrough`) | Plain reads; CAS only on write |
| Wakes the peer | Yes (metadata-only notification) | Never |
| Typical use | "Review round 2 is ready for you" | `activity.rust-suite`, freeze fingerprints, gate status |

Convention: coordinate *who does what next* through the mailbox; publish
*what is currently true* on the board. A mailbox message may simply say
"board key `gates.union` changed — read it when convenient."

See [SQLite session storage](sqlite-session-storage.md) for the two-database
layout, migration, and boot ordering.
