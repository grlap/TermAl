# Durable agent mailboxes

TermAl peer coordination uses durable neutral mailboxes. A mailbox is not an
agent session: it has no assigned agent, runtime, model, workdir, composer, or
turn lifecycle. It is an ordered conversation record shared by root sessions.
See [Agent delegation sessions](agent-delegation-sessions.md#peer-session-connections)
for root-peer eligibility and the boundary between this shipped mailbox flow
and the older connection-oriented proposal.

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

The wake-up prompt is not the message. If wake-up fails, the committed message
remains available and its receipt reports `durableButNotWoken`. Before ordinary
local dispatch, TermAl restores only these never-woken notifications. At boot,
it performs one broader pass over every unread inbound mailbox so a previously
delivered notification whose agent turn died in the crash is not stranded.
Once a recovery wake is durably queued it is marked `recoveredWake`; starting
that wake and having its runtime command channel accept it marks the covered
notifications delivered. A rejected runtime send leaves them recoverable.
Completion without acknowledgement does not create another autonomous turn,
while the message remains unread until the participant explicitly advances its
cursor.

Recovery is bounded to 16 mailboxes per pass; the complete authoritative list
remains available through `termal_list_mailboxes`.

## Idempotency

Idempotency keys are unique per sender session.

- Retrying the same key with the same target and exact message intent returns
  the original receipt with `duplicate: true`. It does not insert or wake
  twice. Participant display names are mutable snapshots and do not change the
  stable intent comparison; the original stored names remain authoritative.
- Reusing the key with a different target, body, topic, or state stamp is a
  conflict.

This protects callers from ambiguous network outcomes without silently
replacing earlier messages.

Message bodies are limited to 256 KiB. Optional `topic` and `stateStamp`
metadata values are each limited to 4 KiB. Oversized values are rejected
instead of truncated.

## Reading and acknowledgement

Mailbox reads are pull-based and ordered by sequence. Fetching never mutates a
participant cursor, and opening the inline mailbox viewer from a conversation
link is always read-only. Each open resolves the mailbox's current latest
sequence and fetches the newest bounded window, so an old notification link does
not pin the viewer to stale history.

Acknowledgement is a forward-only compare-and-swap:

- `expectedProcessedThrough` is the cursor value the agent observed through
  `termal_list_mailboxes` in its own participant entry.
- `processedThrough` is the last sequence it processed.
- A stale expected value conflicts instead of overwriting another reader's
  progress.

## Foundation scope

The foundation supports `routine` messages only. `stop` or urgent delivery is
rejected until the explicit interrupt semantics in `tm-uwx.3` are implemented;
ordinary durable delivery must not imply a safety guarantee it does not have.

One compact wake-up prompt is retained per receiver/mailbox whenever the
receiver is busy or already has queued work. New sends update that prompt's
metadata while every body remains independently ordered and durable in SQLite.

## Storage and shutdown

Mailboxes use normalized SQLite tables (`mailboxes`,
`mailbox_participants`, and `mailbox_messages`) through one long-lived
connection configured with WAL, `synchronous=NORMAL`, and the existing
five-second busy timeout. Mailbox operations bypass the asynchronous AppState
persist worker and remain usable after that worker shuts down.

See [Architecture](../architecture.md) for the system-level API and persistence
overview.
