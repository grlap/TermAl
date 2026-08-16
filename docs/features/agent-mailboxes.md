# Durable agent mailboxes

TermAl peer coordination uses durable neutral mailboxes. A mailbox is not an
agent session: it has no assigned agent, runtime, model, workdir, composer, or
turn lifecycle. It is an ordered conversation record shared by root sessions.
See [Agent delegation sessions](agent-delegation-sessions.md#peer-session-connections)
for root-peer eligibility and the boundary between this shipped mailbox flow
and the older connection-oriented proposal. Mailboxes are the EDGE half of
peer coordination — messages that activate sessions; the LEVEL half —
persistent versioned facts that never wake anyone — lives on the
[coordination board](agent-boards.md).
Both durable coordination surfaces live in the isolated database described by
[SQLite session storage](sqlite-session-storage.md).

## Delivery contract

`termal_send_to_session` accepts a peer session id or name, a message body, and
a required sender-supplied `idempotencyKey`.

1. TermAl resolves and validates both participants as local root sessions.
2. The body is committed to SQLite with the next dense mailbox sequence.
3. Only after commit, TermAl best-effort wakes the receiver with mailbox
   metadata (mailbox id, latest sequence, and unread count).
4. The receiver explicitly fetches bodies with `termal_read_mailbox` or
   `termal_read_mailbox_message`.
5. After processing, the receiver advances its cursor with
   `termal_acknowledge_mailbox`.

Mailbox participation follows the live local-root session record. Deliberate
session deletion is the only operation that evicts a participant by setting
`left_at`. Send-time liveness probes affect wake delivery only and never
rewrite participation. For compatibility with rows incorrectly evicted by
older builds, the first eligible send, list, read, exact-message read, or
acknowledgement automatically clears stale `left_at` markers, then revalidates
the live session so a concurrent real deletion remains authoritative. This
repair preserves the existing session id, transcript, mailbox history, and
processed cursor.

The same local-root requirement applies directly at the list, read,
exact-message-read, and acknowledgement REST routes. Hidden sessions, remote
proxies, and delegation children receive `400` and cannot use those routes to
cross the root-peer boundary. The TermAl MCP bridge additionally hides and
rejects all peer/mailbox tools for delegation children, so a well-behaved child
never reaches these route errors during normal tool discovery or invocation.

### Structured delegation review results

Delegated `/review-code` uses one deliberately narrower mailbox capability:
`termal_submit_review_result`. It is not peer messaging. The tool accepts only a
versioned review-result object and exposes no target, topic, state stamp, or
idempotency key. TermAl derives those fields from the durable delegation link,
accepts the call only from a linked reviewer-mode child for which the host
requires the structured-result protocol, and routes it only to that child's
own root coordinator. TermAl injects this requirement when it creates the
reviewer delegation; repository commands do not opt in and need no marker.

The stored message uses topic `delegation-review-result/v1`, the delegation id
plus backend-owned submission-attempt number as its state stamp, and a
deterministic per-delegation/per-attempt idempotency key. The attempt advances
when a completed delegation is rearmed for follow-up. Nested objects reject
unknown fields; severities, statuses, sizes, and list counts are validated
before append. An exact retry returns the original receipt. A second, different
result for the same attempt conflicts.

Review-result messages are committed with
`notificationDisposition: durableButNotWoken`. They do not create an early
mailbox turn in the coordinator: the existing delegation wait remains the only
fan-in activation. The versioned control-plane topic is also excluded from
routine mailbox unread counts, latest previews, reads, and both wake-up queries.
This filtering never advances a participant cursor; durable recovery reads the
stored envelope directly by its backend-owned idempotency key. After the child
reaches a terminal state, the backend
promotes the validated submission into the compact delegation result. Submission
is authoritative; subsequent runtime failure, idle teardown, or child disappearance
is recorded as separate transport metadata and cannot replace the submitted fields.
Missing required submissions still fail closed as unavailable. Explicit
cancellation retains a previously accepted envelope without promoting it. The child
transcript remains available as human-readable full output but is not a protocol
source.

The child-facing ingest path stays strict, while parent-side crash recovery is
resilient. A corrupt or identity-mismatched durable review envelope is logged,
quarantined for that submission attempt, and reported through
`reviewResultRecoveryError`; it never bricks delegation status, result paging,
cancellation, or follow-up with a repeated `500`. Recovery probes live on the
delegation record and are compared with the backend-owned attempt number. A
narrow coordination-store guard serializes the recovery read with a structured
result append without holding the main application-state mutex across SQLite
I/O.

The wake-up prompt is not the message. If wake-up fails, the committed message
remains available and its receipt reports `durableButNotWoken`. Before ordinary
local dispatch, TermAl restores never-woken notifications. If a materialized
recovery wake is rejected by a runtime command channel, that exact mailbox wake
is requeued without recursively draining the failed session's prompt queue. At
boot, TermAl performs one broader pass over every unread inbound mailbox so a
previously delivered notification whose agent turn died in the crash is not
stranded.
When recovery queues a never-woken row it advances from
`durableButNotWoken` to `recoveredWake`. A boot pass may also recreate a wake
for an unread row already marked `deliveredToIdleSession`; its mutable
`notificationState` returns to `recoveredWake` while the immutable original
receipt outcome remains `deliveredToIdleSession`. Boot recovery only
materializes the visible queued prompt: restart itself never dispatches a
mailbox-only queue. The next genuine activation of that session drains the
recovery wake first. A committed delegation-wait or orchestrator resume is
itself a durable activation; if one is already queued behind a boot-recovered
mailbox wake, startup drains the mailbox wake first and then continues the
workflow normally. An ordinary user prompt already at the queue head remains an
activation barrier: restart does not skip it merely because a workflow prompt
is queued behind it.
Starting a recovery wake and having its runtime command channel accept it marks
all covered notifications delivered. A rejected runtime send leaves them
recoverable.
Completion without acknowledgement does not create another autonomous turn,
while the message remains unread until the participant explicitly advances its
cursor.

Ordinary pre-dispatch recovery is bounded to 16 mailboxes per pass. The
one-time boot pass covers every unread inbound mailbox because delivered turns
that died in the crash have no narrower recovery path. The complete
authoritative list remains available through `termal_list_mailboxes`.

### Dispatch outcome versus notification state

The send receipt and a later mailbox read intentionally expose different
fields:

- Receipt `notificationDisposition` is the immutable point-in-time outcome of
  the original send attempt: `deliveredToIdleSession`,
  `queuedBehindActiveTurn`, or `durableButNotWoken`.
- Message `notificationState` is the current persisted wake lifecycle state:
  `durableButNotWoken`, `queuedBehindActiveTurn`, `recoveredWake`, or
  `deliveredToIdleSession`.

The SQLite column `notification_disposition` is the legacy storage name for
this mutable `notificationState`; it is not the immutable receipt disposition,
which is stored separately in `dispatch_outcome`.

Every row commits first as `durableButNotWoken`. A successful initial dispatch
records its receipt outcome and advances the row to the corresponding state. A
failed wake remains recoverable; recovery advances the state to
`recoveredWake`, and runtime acceptance advances every covered row to
`deliveredToIdleSession`. Consequently, a read taken inside the
commit-before-notify window can show `durableButNotWoken` even when the sender
subsequently receives `queuedBehindActiveTurn`, and a later read can show
`deliveredToIdleSession`. Initial outcome recording only advances a row that is
still `durableButNotWoken`; it cannot regress a recovery or runtime-delivered
state. This is state convergence, not a receipt mismatch.

## Idempotency

Idempotency keys are unique per sender session.

- Retrying the same key with the same target and exact message intent returns
  the original immutable dispatch receipt with `duplicate: true`, even after
  the message's `notificationState` advances. It does not insert or wake twice.
  A concurrent retry that arrives while the original request is finalizing its
  dispatch outcome waits for that finalization without holding the SQLite
  writer slot, subject to the same five-second request deadline; expiry returns
  a retryable `503`. After a restart, a legacy pending outcome has no live
  finalizer and resolves conservatively to the append-boundary
  `durableButNotWoken` receipt.
  Participant display names are mutable snapshots and do not change the stable
  intent comparison; the original stored names remain authoritative.
- Reusing the key with a different target, body, topic, or state stamp is a
  conflict.

This protects callers from ambiguous network outcomes without silently
replacing earlier messages.

Message bodies are limited to 256 KiB. Optional `topic` and `stateStamp`
metadata values are each limited to 4 KiB. Oversized values are rejected
instead of truncated.

## Reading and acknowledgement

Mailbox reads are pull-based and ordered by sequence. Fetching never mutates a
participant cursor. Human mailbox notifications render as a compact
sender/preview/unread link; the agent-only list/read/ack activation text remains
stored but is not shown as the human card body. The link opens a dedicated
read-only workspace tab with no agent, runtime, model, workdir, or composer.
Workspace state deduplicates by mailbox id, so every notification for the same
mailbox focuses one tab instead of creating copies.

The tab resolves the mailbox's current latest sequence and fetches 50 messages
at a time, newest first. Primary processed/unread state is derived from the
target participant's `processedThrough` cursor; wake lifecycle state is
diagnostic metadata shown only in an expanded row. A divider marks each visible
lagging participant boundary. Opening, expanding, paging, and closing use only
summary/range reads and never call acknowledgement or otherwise advance an
agent cursor.

Acknowledgement is a forward-only compare-and-swap:

- `expectedProcessedThrough` is the cursor value the agent observed through
  `termal_list_mailboxes` in its own participant entry.
- `processedThrough` is the last sequence it processed.
- A stale expected value conflicts instead of overwriting another reader's
  progress when the requested cursor has not already been reached. Replaying
  an acknowledgement whose `processedThrough` is already satisfied succeeds
  idempotently, which recovers a commit whose response was lost.

## Foundation scope

The foundation supports `routine` messages only. `stop` or urgent delivery is
rejected until explicit interrupt semantics are implemented; ordinary durable
delivery must not imply a safety guarantee it does not have.

One compact wake-up prompt is retained per receiver/mailbox whenever the
receiver is busy or already has queued work. New sends update that prompt's
metadata while every body remains independently ordered and durable in SQLite.

## Storage and shutdown

Mailboxes use normalized SQLite tables (`mailboxes`,
`mailbox_participants`, and `mailbox_messages`) through one long-lived
connection to `~/.termal/coordination.sqlite`, configured with WAL,
`synchronous=NORMAL`, and the existing five-second busy timeout. The
coordination board uses a second long-lived connection to the same small
database and shares its per-file FIFO writer admission. Session and transcript
persistence stays in `termal.sqlite` with a different admission domain, so a
large active transcript cannot block mailbox or board writes.

Mailbox operations bypass the asynchronous AppState persist queue and remain
usable after that worker shuts down. Request-owned append,
acknowledgement, and duplicate-finalization waits use a five-second deadline;
if it expires, the backend returns `503 Service Unavailable`. Append and
acknowledgement admission failures explicitly confirm that the attempted
operation did not commit a mailbox write. Internal post-commit lifecycle
updates wait through the short in-process writer boundary because no external
caller can safely replay them. WAL still permits concurrent readers.
External-process or OS-level lock exhaustion uses the same retryable
classification instead of an internal-error `500`.

On the first boot of this layout, TermAl attaches the legacy `termal.sqlite`
read-only and copies mailbox plus board rows into `coordination.sqlite`. Copy,
verification, and the destination-owned completion marker commit in one
destination transaction before either coordination store or the HTTP listener
is available. Legacy coordination tables remain inert and are not deleted.

While an original dispatch is still finalizing, duplicate requests may receive
repeated retryable `503` responses; retrying the same idempotency key remains
safe throughout and eventually recovers the original immutable receipt. The
MCP bridge opts mailbox send/list/read/acknowledgement calls into bounded
safe-replay handling; generic bridge requests remain single-attempt, so future
non-idempotent endpoints cannot inherit replay from error wording alone.

The MCP bridge sends an exact `session-*` target directly to the mailbox
endpoint; backend target validation remains authoritative. The bridge performs
one fail-safe root/child eligibility lookup after the caller appears in state
and caches that immutable caller classification for its lifetime. TermAl no
longer creates hidden prewarmed Claude sessions, so every live caller is either
a visible root or a delegated child. Transient lookup failures still fail
closed without caching. This avoids a full-state snapshot per exact-id message
during sustained coordination.
Case-insensitive names remain
supported, but require `/api/state` resolution, so callers should retain and
reuse exact ids returned by `termal_list_sessions`.

Within the current local single-user trust model, exact-id failures retain the
backend distinction between missing and ineligible targets: only root callers
can reach that resolution path, and those callers can already enumerate peers.
If peer tools gain multi-user or remote exposure, this diagnostic tradeoff must
invert and all missing, self, and ineligible target failures must be normalized
to one generic error.

An HTTP `409` or `503` response remains typed and retains the backend error
body through the MCP bridge. A transport failure after a request was sent, or
an unusable successful response whose receipt/summary cannot be validated, is
different: the bridge cannot know whether SQLite committed before the response
was lost or corrupted. Send diagnostics therefore label the append outcome
`unknown` and instruct callers to retry the exact same intent and
`idempotencyKey`; a committed
first attempt returns its original receipt with `duplicate: true`.
Acknowledgement constructs its returned summary inside the same transaction
as the cursor update, so the backend has no fallible response lookup after
commit. Transport failures still instruct callers to re-list the mailbox
cursor before constructing the next compare-and-swap; replaying an already
satisfied acknowledgement is safe.

The mutable row lifecycle remains stored in the historical
`notification_disposition` column for database compatibility. A separate
immutable `dispatch_outcome` column preserves duplicate receipt semantics.
Existing rows are backfilled into the documented immutable receipt domain:
`queuedBehindActiveTurn` and `deliveredToIdleSession` remain exact, while
`recoveredWake` or any unknown historical lifecycle value maps conservatively
to `durableButNotWoken` because it cannot reconstruct the original
point-in-time dispatch outcome. Preserving a legacy
`deliveredToIdleSession` value is necessarily an approximation: the historical
single column cannot distinguish direct delivery from delivery reached after a
recovery wake. Do not use pre-migration receipt outcomes to derive recovery
statistics.

See [Architecture](../architecture.md) for the system-level API and persistence
overview.
