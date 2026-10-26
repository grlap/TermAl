# Native cross-host routine mailbox relay

Native relay wake requires the satellite to be Active at the central.
Verification alone permits durable mail without activating the host; its wake
remains held. Remote native names carry `nativeWakeRequiresActivation` in the
MCP listing, and `durableButNotWoken` receipts explain the central operator's
activation remedy. This prerequisite label does not assert current membership.
The relay's existing admission predicate remains the only wake authority.

The existing `termal_send_to_session` / CLI `mailbox send` addresses a remote
native root as `host:<host UUID>::<native session id>`. A bare `session-*` id
always selects a local root. A missing local id is refused with guidance for
the explicit remote form. A UI proxy cannot initiate a new remote send; its
refusal gives the native address when the enrolled peer is known.
This sends routine durable mail through the existing native mailbox relay.
The native relay requires the enrolled central/satellite pair. A satellite's
MCP listing uses bare ids for its local roots, the explicit host/native form
for its central, and canonical domain ids for sibling satellites. Those
canonical ids use domain mail and its separate wake-authority contract; they
do not promise a native relay wake. An explicit native address without a direct
pair is refused without a routing fallback.
Its normal mailbox wake can start the recipient's turn; it grants no executor
placement, enrolls no session in a mailbox domain and moves no local history.

## Identity and prerequisites

Both hosts must support version 1 of `/federation/v1/mailbox/relay`, with an
already enrolled, enabled, qualified and authenticated host pair. The explicit
host UUID selects the enrolled peer; an authenticated RPC proves the exact
native target. Display names and SSH host names alone grant no authority.
A missing pair, disconnected transport, unsupported peer, changed identity or
ambiguous locator is an explicit refusal, with no local mailbox or executor
fallback. Installing this code on only one host does not enable the relay.

A principal includes host UUID/generation, coordination-store UUID, native
session ID and its creation UUID. Hidden roots, delegation children, proxies
and missing sessions cannot act as native principals. Equal `session-N` values
on different hosts are distinct. A replaced store or session cannot inherit
an old mailbox. Both originating-host authentication and conversation
membership are checked; a copied handle is not a capability.

## One home, unchanged retries

The pair's configured central host is the single immutable conversation home,
including when the satellite sends first. Both directions share that mailbox.
Bodies, dense sequences, issued read receipts and participant cursors live
only in the home's coordination database. Satellite reads and acknowledgements
require the home; there is no offline body replica or cursor merge.

Before a send can have an uncertain result, its source retains metadata:
original requested target, native principals, home/store, peer transport
basis, payload digest and idempotency key. It retains no payload copy. After a
lost response or source-store reopen, retry the exact same request and key.
The home returns the original message with `duplicate: true`; changed content,
target, native identity or enrollment conflicts. Turning a proxy into a local
record cannot redirect a pinned request to a standalone mailbox. These pins
do not promise automatic crash resend without the original caller's payload.
A fresh link may advance transport/accounting generations without changing
that logical request; each retry still needs current mutual authentication,
the original enrollment material and exact native/store identities.

## Tools, handles and acknowledgements

The ordinary send/read/list/exact-message/acknowledge routes and tools remain
the interface. Relay mailbox IDs start with `relay-mailbox-`; exact message
IDs start with `relay-message-`. They encode a versioned home location, not
authorization. Use them unchanged. A message's qualified `relay-principal-`
sender can be used as the reply target by its conversation peer.

List includes known relay homes alongside the caller's existing mailbox
surface. A source pin can recover its home association through list even when
the original send reply was lost. An unavailable known relay home is reported
as unavailable/unverified in the partial-list projection or legacy response
headers (see below), never as an empty inbox; explicit standalone
mailbox reads remain local.

A first recipient can read an exact `relay-mailbox-*` handle before receiving
its wake. If it has no local association yet, the read lists that exact
original home over the authenticated pair, proves the caller's native
membership and remembers the immutable association before reading. Recovery
and a concurrent wake share the existing association writer and preserve its
ACK cursor. An unrelated root cannot recover membership from a copied handle.
An ordinary list with no known home or locator cannot discover an unknown
home store; the exact handle or the normal metadata wake supplies that location.

Reads do not acknowledge. MCP/CLI request issuance; acknowledgements accept
only the unchanged issued receipt, never a numeric boundary or `readKey`.
`noContinuation` is refused for relay mailboxes. Relay receipts conservatively
report `durableButNotWoken`: commitment is not proof that a native agent ran.
The home's message notification field also remains conservative; native wake
handoff metadata is not a replicated mailbox cursor or an executor result.

## Wakes and recovery

The home atomically commits a metadata-only wake obligation with the body.
Its joined worker delivers bounded wake batches over the authenticated pair.
The receiver retains notification metadata and uses its normal queue and
handoff guards. No message body is copied into a wake. Accepted wakes coalesce;
acknowledging mail keeps an owed continuation while removing its unread claim,
including an ACK that precedes a delayed wake. A failed handoff/turn restores
the same native boundary and obeys existing paused-queue behavior.
For an acknowledged continuation that has never reached the runtime, recovery
requires the exact first enqueue to be known rejected; ACK, an unknown outcome
or an already accepted handoff is not permission to replay it.
Duplicate wakes and live recovery scans do not recreate an explicitly cancelled
queued notification. As with ordinary unread-mail boot recovery, still-unread
mail can produce a new notification after restart; cancelling a prompt is not
acknowledging its mailbox. An acknowledged continuation is not reconstructed
from a previously queued metadata marker alone after restart.

Wake admission checks each host's own record of the other. Central requires
its record of the satellite to be active. A satellite never activates or
requalifies its record of central, which stays enrolled after enrollment and
after every reconnect. It accepts its configured central as the satellite
executor authority does: the same enrollment generation over the current
link, with no quarantine, and short of revocation. Revocation, a changed
enrollment, quarantine or a replaced target still hold the wake.

A home wake that its target refuses keeps its obligation and is retried on
each worker pass. The home logs it once per change of refusal and lists it at
`GET /api/diagnostics/mailbox-relay-wakes`: `pendingHomeWakes` (undelivered
obligations), `refusedHomeWakes` (message and mailbox ids, target host and
session, attempts, last status and a bounded reason with control characters
replaced, first and last refusal times) and `untrackedRefusals`, the refused
attempts not kept once the list is full. These records are process-local and
metadata only, at most 256 entries, and a delivered wake clears its own.

New sends, wake admission and handoffs recheck current native identity and
authority. Store contention or transport loss is not success, nor proof that
an already admitted write did not commit. Retry the original send/key, read
request or acknowledgement receipt; never mint a replacement key to recover
an uncertain write. No automatic network resend of message bodies is added.

## Storage and validation scope

Coordination reader floor `5-mailbox-relay-1` adds the independent store UUID,
associations, sender pins, home wake obligations and native notification
metadata. Exact historical schemas 1–5 upgrade atomically through their
existing steps. Current-target partial/malformed layouts are refused without
repair, and old readers reject the new floor. Primary-state and independent
federation profile versions are unchanged.

Repository controls use disposable real SQLite stores and actual mutually
attested MAC-authenticated loopback listeners, including public two-host API
routing, opposite-first sends, root eligibility, membership/handle isolation,
late/lost replies, fresh-link recovery and unchanged local history. Store
reopen is not an operating-system process-kill test or evidence of a live Mac
installation. Live availability depends on compatible running hosts and their
actual enrolled pair.

## Related contracts

See [agent mailboxes](agent-mailboxes.md#cross-host-access), the
[central control plane](central-control-plane.md) and
[SQLite session storage](sqlite-session-storage.md).

## Availability and local ownership

An identity-bound local root can list, read and receipt-acknowledge its own
authoritative home-local relay history even when the peer is disconnected,
disabled or revoked. This does not authorize sending or provider wake handoff.
Remote-home operations still require current authenticated authority; there is
no local history fallback.

An otherwise authorized wake already committed at its local home does not need
a live peer channel merely to run locally. Revoke, disable, changed enrollment
or native/store replacement still holds it before promotion and final handoff.
The selected carrier is owner-bound across off-lock preparation and rechecked
at promotion; a stale worker cannot start a successor exposed by cancellation.
That owner is captured under the first queue-admission lock too, not only on
later redrive. A process-local admission witness bridges a failed coordination
queue-marker write: a successful live Cancel cannot be undone by a later scan.
It grants no handoff authority, contains no message body, and is not a boot
tombstone; authoritative unread recovery after restart remains separate.
An unavailable relay head is a scheduling hold, not a fatal runtime error:
turn completion remains successful and the shared provider reader stays live.
On an idle root, an explicit ordinary user prompt may pass an exact refused
metadata carrier while preserving that carrier and FIFO among ordinary users.
This does not bypass durability, capacity, Stop/pause, retained authorization
or unsettled exclusive preparation. Relay work remains queued for its bounded
redrive when its current authority returns; it is not acknowledged or cancelled
to make ordinary work progress.

`GET /api/sessions/{id}/mailboxes?includeRelayAvailability=true` returns
`mailboxes` (fresh authoritative summaries) and `unavailableRelayHomes` (home,
availability, status and error, with no invented unread count or empty history).
If a root also has an unavailable central-domain binding, a partial relay list
includes `unavailableCentralDomain` (central identity, availability, status and
error). This preserves the domain refusal without hiding independently owned
relay history; it never falls back to standalone history or authorizes domain
operations. With no relay history, the original domain refusal is unchanged.
The MCP list tool requests this projection. The default HTTP list remains an
array; when remote homes are omitted it includes
`x-termal-unavailable-relay-homes`; an unavailable domain is indicated by
`x-termal-unavailable-central-domain`. Either case includes a Link to the
detailed projection.
