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
Mailbox wakes that enter premium local Engram control retain the delivery and
Stop restoration rules described by the
[Engram Host Adapter](engram-host-adapter.md#mailbox-and-stop-behavior).
Agents whose MCP configuration is locked can drive the same send, read and
acknowledge operations from the shell through the
[coordination CLI](coordination-cli.md), which reuses the MCP bridge contracts.
A next step that must proceed even when its coordinator misses a wake is
recorded in the [handoff ledger](handoffs.md) instead.

## Delivery contract

`termal_send_to_session` accepts a peer session id or name, a message body, and
a required sender-supplied `idempotencyKey`.

HTTP send receipts, `termal_send_to_session`, and CLI `mailbox send` also
return `senderProcessedThrough` and `senderCursorAdvanced`. The first is the
sender's durable cursor snapshot resulting from this operation; the second
says whether this append advanced it. The cursor advances over the sender's
own new message only when it already sat immediately before that message.
An unread inbound gap prevents auto-advance; sending cannot skip that gap.
For legacy numeric acknowledgements these fields refresh the expected cursor
without another list call. They are snapshots, not protection against a
concurrent send or acknowledgement.

1. TermAl validates a native root caller and a root peer. Local conversations
   retain their existing storage. An explicit `host:<host UUID>::<session id>`
   selects the exact remote root through the
   [authenticated native relay](native-mailbox-relay.md). Bare session ids
   remain local; UI proxies cannot initiate remote sends.
2. The body is committed to SQLite with the next dense mailbox sequence.
3. Only after commit, TermAl best-effort wakes the receiver with mailbox
   metadata (mailbox id, latest sequence, unread count, latest inbound sender,
   and topic when present).
4. The receiver explicitly fetches bodies with `termal_read_mailbox` or
   `termal_read_mailbox_message`.
5. After processing, the receiver advances its cursor with
   `termal_acknowledge_mailbox`.

The agent-facing wake has one shape for fresh, coalesced, and recovered
notifications. It contains metadata and one short read/acknowledgement pointer,
never the message body or a repeated protocol tutorial:

```text
[TermAl mailbox notification]
Mailbox `mailbox-example` has 2 unread message(s). Latest inbound: #7 from Sol.
Topic: architecture
Read: `termal_read_mailbox`; acknowledge after processing.
```

The topic line is omitted when no topic is present. Sender and topic display
values are whitespace-collapsed, stripped of control characters and invisible
direction/format marks, and capped at 160 characters plus an ellipsis; the
durable metadata is unchanged. The protocol is taught by the MCP tool
descriptions, coordination CLI help, this document, and managed host instructions,
not appended to every wake. One agent-neutral body in
`src/coordination_instructions.rs` mirrors this prose contract and is consumed
verbatim by the Codex `AGENTS.md` section, Claude initialize's
`appendSystemPrompt`, and a separate text block after the user's task in the
first `session/prompt` for a fresh Gemini, Cursor or OpenCode conversation
(see [OpenCode repository instructions](opencode-acp-integration.md#repository-instructions)).
The block starts with `TermAl host guidance`;
it does not change the resolved task, stored transcript or prompt history,
and never copies repository instruction files. Claude's base system prompt
and user/project/local settings discovery are preserved.

Claude and ACP injection checks the live local-root record: hidden sessions,
remote proxies and delegation/reviewer children receive no root teaching.
Shared Codex homes contain a conditional root-only instruction, not an
invitation for their delegated children to coordinate; child tool guards
remain authoritative. ACP teaching is once per runtime process for a fresh
external conversation, after a successful wire write/flush. Later prompts omit
it. External-session resume/load omits teaching: the provider's conversation
history retains the first turn's block. A fresh `session/new` teaches again;
Claude initialize still supplies its system guidance on every runtime spawn.
Failed ACP writes do not consume the latch. Partial writes cannot guarantee
exactly-once provider delivery. The teaching latch is process-local, not a
durable acknowledgement from the provider.

The shared bootstrap body (bounded to 12 lines and 900 characters) is:

```text
TermAl root coordination; not for delegation children.
Read: termal_read_mailbox; omit afterSequence, save receipt. Reading never acknowledges.
Process bodies in order; reply: termal_send_to_session, stable idempotencyKey; retry identical intent/key.
Ack: termal_acknowledge_mailbox with mailboxId and unchanged receipt after processing the whole page, even after sending. Gap: re-read.
Pages: hasMore/nextAfterSequence. Own sends are returned too. Receipt proves issuance only.
CLI: TERMAL_CLI (PowerShell: & $env:TERMAL_CLI; POSIX: "$TERMAL_CLI").
TERMAL_SESSION_ID / TERMAL_BASE_URL supply identity/URL; never impersonate.
mailbox read --mailbox-id <id> --json (omit --after)
mailbox send --to <id> --message <text> --idempotency-key <key> --json
mailbox acknowledge --mailbox-id <id> --receipt <receipt> --json
mailbox list is discovery only.
```

Mailbox participation follows the live local-root session record. Deliberate
session deletion is the only operation that evicts a participant by setting
`left_at`. Send-time liveness probes affect wake delivery only and never
rewrite participation. For compatibility with rows incorrectly evicted by
older builds, the first eligible send, list, read, exact-message read, or
acknowledgement automatically clears stale `left_at` markers, then revalidates
the live session so a concurrent real deletion remains authoritative. This
repair preserves the existing session id, transcript, mailbox history, and
processed cursor. This is an explicitly deferred internal compatibility path,
not current-schema normalization: removal requires assessing live coordination
rows first. The final audit does not remove it or claim that all internal
compatibility paths are gone.

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
is requeued without recursively draining the failed session's prompt queue. The
same exact-delivery rule applies when a runtime accepts the command but the turn
later fails or is explicitly stopped: TermAl records the mailbox boundary on
that active turn, restores it before draining any successor prompt, and never
uses a session-wide recovery scan that could duplicate a newer delivery. At
that point the rejected-accepted-dispatch recovery pass pauses automatic
dispatch until an explicit user resume or a new
user action explicitly recovers the queue. This keeps a poisoned mailbox wake
from looping and intentionally pauses all already-queued work behind it,
including user prompts as well as automatic work. At
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

The additional injected host guidance is:

```text
ACK alone never suppresses a queued continuation. The host may cover it with an exact read and receipt ACK in the same later active turn, through its boundary, when nothing is unread. Set noContinuation (CLI --no-continuation) on a receipt acknowledgement only when you have fully handled every message through that receipt, sent every reply they need, and owe no work in a later turn. Never set it when you promised a follow-up, are waiting for a delegation, evaluator or gate result that will not arrive as mail, or have not read the whole page. It covers only that mailbox and receipt boundary; a later message, including a reply you await, re-arms the wake. Numeric ACK cannot carry it.
```

### Coverage of an accepted continuation

ACK alone never suppresses an accepted wake. Two independent proofs can cover
its exact mailbox boundary before turn-end checkpoint selection:

- A page is read and that exact receipt is acknowledged in the same active
  runtime turn, whose generation is later than acceptance or the last boundary
  advance. The page must include the boundary and the mailbox must have no
  unread messages. Re-reading an existing receipt records a new read observation.
- A receipt ACK with `noContinuation: true` (CLI `--no-continuation`) explicitly
  declares that no later-turn work remains for that receipt's mailbox boundary.
  The whole page must be handled and required replies sent. Do not use it for
  promised follow-ups, unread bodies, or pending delegation, evaluator or gate
  results that will not arrive as mail. Numeric ACK cannot carry this declaration.

The latest successful declaration per receipt is audited atomically with the ACK in coordination
schema 5, naming session, mailbox, receipt, through-sequence, turn and time.
An outside-turn declaration records generation zero and supplies no coverage.
Exact schemas 1 through 4 migrate additively without changing messages, cursors,
read receipts, delivery records or the landed version-4 executor journal.
Refused or failed ACKs record no declaration. Coverage evidence is process-local:
neither durable cursors nor the audit reconstruct it after restart. Read and ACK
must share a live turn; legacy numeric ACK, preview reads, partial pages and
outside-turn observations do not provide coverage. The flag does not extend to
another mailbox, delegation completion, background completion or evaluator fan-in.

Every boundary advance invalidates coverage, and later mail (including a reply
the sender awaits) re-arms the wake. Retained and durability-held entries, Stop
and paused queues remain protected. A covered removal preserves unrelated queued
work, emits one bounded diagnostic (`session`, `mailbox`, `boundary`, `proof`) and
increments a process-local counter once. Removing the last obligation makes the
checkpoint choose `wait`; a previously retained checkpoint is never rewritten.

### Plain acknowledgement keeps an accepted wake

A message accepted while its receiver is running a turn queues a wake behind
that turn (`queuedBehindActiveTurn`, or `heldBehindPausedQueue` behind a
paused queue). The receiver often reads and acknowledges that message within
the turn that is still running. Plain acknowledgement advances the data cursor only.
Without either coverage proof, the accepted wake keeps its queue position,
identity and boundary, and the
turn after the current one still starts. An Engram-controlled root's
turn-end checkpoint therefore says `continue`, not `wait`.

On acknowledgement, through either the receipt or the numeric route, each
covered queued wake is rewritten so it no longer claims unread mail:

```text
[TermAl mailbox notification]
Mailbox `mailbox-example`: messages through #7 (latest from Sol) were accepted while this wake was queued and are already acknowledged. Nothing is unread.
Topic: architecture
This is the queued continuation wake: resume the next-turn work your context calls for, or end the turn if there is none. Do not send a reply only to acknowledge it.
```

Arrivals coalesce as before: one wake per mailbox, at the newest accepted
boundary. Mail acknowledged from several mailboxes in one turn therefore keeps
one continuation per mailbox, run in queue order. Dispatch-time revalidation
keeps a head with no unread mail as this continuation, rewriting its text if
the acknowledgement-time refresh did not. Its store read is optimistic: it
applies only while the head's prompt, text and source are unchanged, so it
never writes the older unread text back over a head that an acknowledgement
rewrote in the meantime. Without a coverage proof it drops the head only in
two cases: the boundary
row is no longer visible to the receiver, or the row's delivery record
(`notificationState` `deliveredToIdleSession`) shows that a handoff already
covered it. A message delivered directly to an idle receiver queues no wake.
A turn that acknowledges only its own delivered boundary therefore starts no
further turn.

Retained Engram heads are replay input. Acknowledgement neither removes nor
rewrites them, and revalidation skips them. A retained head whose mail is
acknowledged while it is retained is therefore replayed with its original
text, which still names the unread count it had when it was retained. Stop, a
paused queue and explicit Cancel keep their meaning: a held continuation waits
for the explicit resume, and a cancelled one never runs.

This rule covers the normal path only. Failure and restart recovery still
rebuild wakes from unread mail. A continuation whose mail was already
acknowledged is therefore not guaranteed to survive a runtime failure, a
rejected handoff or a host restart before its turn starts.

### Dispatch outcome versus notification state

The send receipt and a later mailbox read intentionally expose different
fields:

- Receipt `notificationDisposition` is the immutable point-in-time outcome of
  the original send attempt: `deliveredToIdleSession`,
  `queuedBehindActiveTurn`, `heldBehindPausedQueue`,
  `waitingForAutomaticRetry`, `waitingForProviderCapacity`, or
  `durableButNotWoken`.
- Message `notificationState` is the current persisted wake lifecycle state:
  `durableButNotWoken`, `queuedBehindActiveTurn`, `heldBehindPausedQueue`,
  `waitingForAutomaticRetry`, `waitingForProviderCapacity`, `recoveredWake`,
  or `deliveredToIdleSession`.

`queuedBehindActiveTurn` means the target is running a turn and the wake waits
for it to end. `waitingForAutomaticRetry` means no turn runs and the target's
queue is held only by a due automatic Engram retry of its queue head (an
admission, begin-preparation, abort or bind retry, see
[the Engram host adapter](engram-host-adapter.md#retained-prompt-recovery)):
the hold clears by itself when the retry is due, and nobody needs to resume
the queue. The session's API state says the same in `queueHold`
(`kind: "automaticRetry"`, the retry's `cause` code and its next `retryAt`),
beside `queuePaused`, which stays true. `heldBehindPausedQueue` means no turn
runs and the target's queue is paused by anything else: a retained Engram
prompt with nothing scheduled, a Stop or an operator's pause, a cancelled
head, an exhausted retry, a settings change, or a failed mailbox turn. Such a
wake waits for that hold to clear, not for a turn to end.

A wake whose own dispatch was held before provider hand-off records
`waitingForAutomaticRetry` as its receipt outcome when an automatic retry now
owns that head, and `durableButNotWoken` otherwise; either way its
`notificationState` stays `durableButNotWoken`, so the never-woken recovery
still covers it.

The SQLite column `notification_disposition` is the legacy storage name for
this mutable `notificationState`; it is not the immutable receipt disposition,
which is stored separately in `dispatch_outcome`.

A receipt therefore says that the message is saved for its recipient and what
the wake looked like AT SEND TIME, never that it was delivered or read, and
never current status. `durableButNotWoken` on a receipt means only that the
wake was unconfirmed when the message was sent; it is not a failed delivery.
Every send result (the HTTP response, `termal_send_to_session` and
`termal mailbox send`) carries `receiptNote`, one plain-language line beside
the unchanged receipt fields. For `durableButNotWoken` it reads:

> Message saved. Wake was unconfirmed when sent; this is not current status.
> Do not resend based on this receipt alone. If current status is needed, use
> termal_read_mailbox_message({messageId: "ID"}) or
> `termal mailbox read-message --message-id ID`.

Any other disposition reads "Wake status AT SEND TIME: <disposition> (<its
sentence>); this is not current status." in the second place. A duplicate
opens with "Duplicate send: the original message was saved earlier." and
always names its receipt's disposition in that AT SEND TIME form. ID is the
receipt's own returned `messageId`: a local id, or the public relay locator
after relay translation, never the home's local id. The line promises no wake
or retry and never asks for a resend or a poll. The CLI prints "stored for …"
with the "wake status at send time", then this line. The host writes the
line; the delegation tool and the CLI pass it on as sent and make none up for
an older host that sends none. A delegated reviewer's review-result receipt
carries no line, because the exact read it would name is for root sessions.

To learn the current state, read the message by its id
(`termal_read_mailbox_message`, `termal mailbox read-message --message-id ID`,
or `GET /api/sessions/{session}/mailbox-messages/{id}`). That exact read
returns `currentStatus`, derived by the mailbox's home in one read and stored
nowhere:
- `observedAt`: when it was read, not when anything changed;
- `notificationState`: the current wake state above;
- `recipientAcknowledged`: the recipient's own cursor has reached this message;
  the sender's cursor never counts, and an acknowledgement is the recipient's
  protocol acknowledgement, not proof that the content was acted on;
- `summary`: "As of <observedAt>: <wake sentence> <acknowledgement>", where
  the acknowledgement is "The recipient has acknowledged it." or "Not yet
  acknowledged by the recipient.", reported apart from the wake. The states
  are not a ladder: a message can be read and acknowledged without any wake,
  and no wording says that a wake never happened. A held state says what it
  waits for (an active turn, a paused queue to be resumed, a due automatic
  retry that clears by itself, provider capacity).
Every message, from a page read or the exact read, also carries
`notificationSummary`, the same wake sentence for its `notificationState`;
the UI shows it beside the state. One formatter
(`src/mailbox_status_text.rs`) writes all of these lines for the API, the
tools, the CLI and the UI.
Page reads carry no `currentStatus`. The exact read issues no page receipt,
moves no cursor and wakes nobody. When a remote mailbox home cannot be reached
(the relay link is down or changed, no relay peer is held, or the transport
gives no usable answer), the read fails with "current status unavailable: the
message's mailbox home could not be reached or gave no usable answer, which
says nothing about delivery; try the read again later (<cause>)", keeping
only the cause's first clause; no receipt or cached state stands in for it.
A local home's own errors, and a membership or enrollment change, are
reported as they are.

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
  the original message identity and immutable dispatch outcome with
  `duplicate: true`, even after
  the message's `notificationState` advances. It does not insert or wake twice.
  Cursor fields are deliberately not immutable: a retry returns the current
  `senderProcessedThrough` snapshot and `senderCursorAdvanced: false`, because
  that retry did not move the cursor.
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

Mailbox reads are pull-based and ordered by sequence. Fetching never advances
a participant cursor. HTTP range reads return
`{messages, afterSequence, processedThrough, receipt, hasMore, nextAfterSequence}`.
MCP and CLI JSON add `mailboxId`; human CLI output prints the token and
pagination fields too. Omit `afterSequence` (CLI: `--after`) to read from
the durable cursor. Explicit boundaries remain useful for replay and paging:
zero replays history, and other values read strictly after that sequence.

HTTP reads default to non-issuing preview (`issueReceipt` omitted or false).
The UI uses that default, including already-open older bundles: browsing cannot
authorize an agent's later numeric ACK. Preview page collection uses a read
transaction without writer admission or issuance writes. The route retains the
existing live-participant validation/reactivation step, which may repair stale
`left_at` markers; that repair does not issue pages or advance cursors.
The MCP/CLI bridge sets `issueReceipt: true`
itself (not a model tool argument) for agent reads.

For agent reads, the server commits an opaque receipt for each nonempty page, bound to the
mailbox, participant and exact returned range. Rows, cursor and issuance share
one transaction. `nextAfterSequence` is the last returned sequence, not a count;
`hasMore` describes additional visible rows in that snapshot. An empty page
has no receipt or next boundary. Own sent messages are returned too; structured
review-result rows hidden from this participant are not returned. Consequently,
visible sequences may have legitimate gaps.

The routine workflow is read -> process/reply -> acknowledge:

1. Read with the mailbox id and no explicit boundary. Save `receipt`.
   Listing is discovery only when the mailbox id is already known.
2. Process the whole page in order. Reply using a stable idempotency key.
   Retry ambiguous sends with the same intent/key; do not recalculate the
   page receipt when sending changes the cursor. A reply appended after this
   read is outside the page. Acknowledge the original page, then read/process/
   acknowledge the next page containing that own reply. A numeric ACK through
   the unissued reply returns `409`; sending does not issue a read page.
3. Call `termal_acknowledge_mailbox({mailboxId, receipt})`, or
   `mailbox acknowledge --mailbox-id <id> --receipt <receipt>`.
   Supply no numeric boundaries with a receipt.
4. Read the next page from the cursor. Prefetching with `nextAfterSequence`
   is permitted, but process and acknowledge pages in order. A later receipt
   cannot skip an earlier unacknowledged visible page, even if both were issued.
   On a gap rejection, read from the current cursor and process that page first.

Receipts survive restart and lost responses. Repeating an identical read range
returns its existing token. Replaying an acknowledged receipt succeeds without
moving the cursor backwards, including after a send or another acknowledgement
has advanced it. An altered token or one issued to another participant/mailbox
is rejected. Issuance proves neither that the response reached the agent nor
that its contents were read, understood, or acted on; processing remains the
agent's obligation. Exact-message lookup is diagnostic and does not issue a
page receipt: use a range read before advancing the cursor.

Human mailbox notifications render as a compact
sender/preview/unread link; the agent-only metadata/pointer activation text remains
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

Legacy numeric acknowledgement remains a forward-only compare-and-swap:

- `expectedProcessedThrough` is the cursor value the agent observed through
  a read response's `processedThrough`, a send receipt's
  `senderProcessedThrough`, or `termal_list_mailboxes` in its own participant
  entry. An explicit read's `afterSequence` is not necessarily that cursor.
- `processedThrough` is the last sequence it processed.
- Every newly acknowledged visible message must have recorded page issuance
  for this participant. Numeric boundaries are not a bypass: an unissued gap
  returns `409`, requiring a new read and processing before retry. Historical
  cursors are preserved on upgrade; unacknowledged reads made before version 2
  must be repeated through an upgraded MCP/CLI bridge. Legacy numeric
  instructions remain supported, but old bridge binaries omit `issueReceipt`
  and therefore only preview: repeating their reads cannot resolve the `409`.
  Upgrade/restart the bridge runtime or use the current CLI. Deployment must
  refresh both server and agent bridge processes; do not assume replacing a
  binary refreshes an already-running bridge. New clients use receipts.
- Process messages contiguously; a send that leaves an unread gap does not
  authorize acknowledging past that gap. Sending a reply can auto-advance the
  cursor, so use its newer receipt snapshot when constructing the next CAS.
- A stale expected value conflicts instead of overwriting another reader's
  progress when the requested cursor has not already been reached. Replaying
  an acknowledgement whose `processedThrough` is already satisfied succeeds
  idempotently, which recovers a commit whose response was lost.

## Cross-host access

The native routine relay is described in
[Native cross-host routine mailbox relay](native-mailbox-relay.md). It accepts
an explicit `host:<host UUID>::<native session id>` through the existing send tool and routes subsequent
reads, replies and receipt acknowledgements to one immutable central home.
Both hosts must implement that protocol and have an enrolled, authenticated
pair with current qualification. No implicit migration of standalone history
or domain enrollment occurs. The ordinary native mailbox wake uses the
recipient's existing queue; it grants no executor placement.

The separately bound central-domain protocol is designed in
[Central control plane, satellite hosts and cross-host mailboxes](central-control-plane.md),
which remains its authoritative contract. Its qualified participants, explicit
bindings and outbox are distinct from native relay principals and metadata pins.
The following summarizes that domain design, not a promise that the native
relay queues payloads offline:

- **One home.** One explicitly configured central TermAl host is the only home
  of every mailbox in its coordination domain. A mailbox is never duplicated,
  mirrored or merged, and no host keeps a replica: satellite hosts serve list,
  read, exact-message read and acknowledgement only by calling the central host
  over the enrolled link, with no local fallback and no offline read.
- **Same contract.** Participants on satellite hosts are first-class: every
  operation of this document keeps its meaning, operation for operation, with
  page receipts issued by the central host and acknowledgement as a
  receipt-only compare-and-swap there. Participants are addressed by qualified
  `host_id/session_id`.
- **Outbox is retry state only.** A sender's host keeps its own unconfirmed
  sends durably for retry; that outbox is never a copy of a mailbox and never
  answers list, read, acknowledgement or cursor.
- **Executor proxies stay a distinct kind.** Remote-backed proxy sessions of
  [Project-scoped remotes](project-scoped-remotes.md) are never conflated with
  native sessions, and enrolling a host never converts them; today's
  executor-proxy path is unchanged until a separately scoped slice says
  otherwise.
- **Trust.** The pilot stays inside the single-user trust model: one operator's
  explicitly enrolled machines over that operator's OpenSSH. If mailboxes ever
  cross a user boundary, the diagnostic normalization requirement at the end of
  this document applies: missing, self and ineligible target failures must
  collapse into one generic error.

A host that cannot reach the central host reads nothing; its own new sends may
queue durably as outbox status, never as mailbox history. Out of scope:
federation between independent control planes, replication and offline merge.

## Foundation scope

The foundation supports `routine` messages only. `stop` or urgent delivery is
rejected until explicit interrupt semantics are implemented; ordinary durable
delivery must not imply a safety guarantee it does not have.

One compact wake-up prompt is retained per receiver/mailbox whenever the
receiver is busy or already has queued work. New sends update that prompt's
metadata while every body remains independently ordered and durable in SQLite.

## Storage and shutdown

Mailboxes use normalized SQLite tables (`mailboxes`,
`mailbox_participants`, `mailbox_messages`, and `mailbox_read_pages`) through one long-lived
connection to `~/.termal/coordination.sqlite`, configured with WAL,
`synchronous=NORMAL`, and the existing five-second busy timeout. The
coordination board uses a second long-lived connection to the same small
database and shares its per-file FIFO writer admission. Session and transcript
persistence stays in `termal.sqlite` with a different admission domain, so a
large active transcript cannot block mailbox or board writes.

Mailbox operations bypass the asynchronous AppState persist queue and remain
usable after that worker shuts down. Request-owned append, page issuance,
acknowledgement, and duplicate-finalization waits use a five-second deadline;
if it expires, the backend returns `503 Service Unavailable`. Append and
acknowledgement admission failures explicitly confirm that the attempted
operation did not commit a mailbox write. Internal post-commit lifecycle
updates wait through the short in-process writer boundary because no external
caller can safely replay them. WAL still permits concurrent readers.
External-process or OS-level lock exhaustion uses the same retryable
classification instead of an internal-error `500`.

Coverage applies to standalone local mailboxes. Central-domain and native relay mailboxes retain
their receipt-only protocol and owed wakes; `noContinuation: true` is refused
there, rather than silently accepted without a matching local runtime proof.

Before either coordination store or the HTTP listener is available, TermAl
initializes an empty `coordination.sqlite` with the current schema, atomically
upgrades exact historical versions 1 through 5 and the relay floor `5-mailbox-relay-1` to
`6-executor-outcomes-1`, adding receipt, delivery, executor, continuation-audit, native relay
and central executor-outcome storage as needed without changing messages,
cursors, delivery or executor records, or validates
that an existing file already has the exact current version and canonical
schema definitions, including column types and constraints, foreign keys, and
named indexes. Empty-state initialization is decided again while holding an
SQLite `BEGIN IMMEDIATE` transaction, so independent TermAl processes cannot
both attempt to create the schema. It does not inspect or attach
`termal.sqlite` for mailbox or board rows. Obsolete unreleased developer
schemas are rejected with a clear move/delete-and-reset instruction instead of
being migrated. SQLite read, lock, corruption, and I/O failures retain their
operational error and the actual database path rather than being mislabeled as
schema drift.

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

The mutable row lifecycle is stored in `notification_disposition`. A separate
immutable `dispatch_outcome` column preserves duplicate receipt semantics.
Both columns are part of the current schema from database creation onward;
there is no legacy dispatch-outcome backfill.

See [Architecture](../architecture.md) for the system-level API and persistence
overview. [Test Runs](./test-runs.md) covers the launcher's `--notify`
completion messages and the host-owned wake that replaces them inside TermAl.
