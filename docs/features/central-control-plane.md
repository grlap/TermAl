# Central control plane, satellite hosts and cross-host mailboxes

## Status

The disabled-by-default foundations implement durable host identity, registry
membership, capacity reservations and explicit placement. The local operator
surface below configures that foundation. Explicitly bound domain participants
use the central mailbox protocol described below; unbound sessions keep
local-only mailbox behavior. Native remote execution remains a later slice;
executor proxies remain the separate mechanism in
[Project-scoped remotes](project-scoped-remotes.md). The rollout below lands
each slice with its own tests, review and gate; enabling it on real machines
is a separate, explicit operator step.

This brief is the accepted cross-host design in the repository's own words. It
replaces the earlier plan, in which remote-backed proxy records joined
mailboxes as local sessions and remote servers relayed coordination through a
configured upstream, leaving remote participants second-class; see
[Durable agent mailboxes — Cross-host access](agent-mailboxes.md#cross-host-access)
for how that page now points here.

## Foundation mailbox protocol

Protocol version 1, schema `1`, uses the authenticated domain-only
`/federation/v1/mailbox` route over the [federation link](federation-link.md).
The bridge and CLI keep calling their local host. A bound satellite session's
send, creation, directory, list, page read, exact-message read and receipt-only
acknowledgement relay to the central host; its local store never supplies them.
Unavailable central reads and sends fail with a retryable `503`, with no local
queue, cached response or fabricated append receipt.

Participation is explicit. A central native session joins through
`POST /api/sessions/{id}/mailboxes/domain`; a satellite's host installs the
central placement binding against its exact native creation identity. Names
and mailbox aliases are discovery only. The central directory returns
qualified ids and refuses ambiguous aliases; creation identity, placement
generation and central authority epoch fence mailbox membership and receipts.
An old binding cannot attach to a recreated native session.

An already-bound satellite root can also be addressed through an explicit
[federation mailbox reference](federation-mailbox-references.md). These fresh,
caller-owned handles are mailbox-only locators, not executor proxies, native
participants or permission to install a binding. Ordinary agent sends resolve
them with fresh proof before using the canonical mailbox API.

Binding never moves existing local mail. An explicit central-native join or
satellite binding install refuses a session with any standalone mailbox history
(`409`, with the count and mailbox ids); use a fresh session instead. A refused
session keeps its existing local reads and acknowledgements. Writer admission
serializes binding against standalone sends, and standalone sends refuse bound
participants. Rejoining an already-bound central-native session changes nothing.
Domain routes accept a directory-returned qualified `sessionId`, a unique
literal name, or an explicit `host:Host::Name` alias; bare `session-N` references remain local-only.
An exact canonical id takes precedence over display aliases, and a missing or
stale canonical id never falls back to a display name.

Domain alias grammar is shared by central, satellite and MCP resolution:
`name:<exact literal>` selects only a literal display name; for example,
`name:Termal::O1-A`. `host:<host label or UUID>::<exact local name>` selects
that host and name; for example, `host:Central::Termal::O1-A`. The first `::`
after `host:` is the separator, and the remaining local name may contain `::`.
Use a host UUID when its label contains `::` or labels are ambiguous. Aliases
compare without ASCII case sensitivity; they never create or rename a session.
Names beginning `name:` or `host:` can themselves be selected with `name:` or
a host-qualified form. `host:<host UUID>::<native session-id>` instead parses
as a typed exact host/native pair, never a name. On domain-only create and
delivery-intent routes it selects an already-current directory participant;
no match returns a refusal, with no name fallback or implicit binding.
Ordinary `/mailboxes/send` and `termal_send_to_session` instead prove that exact
remote native root over the qualified pair and use the existing
[native relay and mailbox wake](native-mailbox-relay.md), without a manual
session join. A UI proxy cannot initiate a new remote send. Native ids require
the host UUID, not a host label. Other principal/reference namespaces remain
reserved inside aliases. Bare native ids remain local-only.

A legacy bare name containing `::` remains a literal alias only when the
central directory is complete, exactly one current literal name matches, and
no known domain host label or UUID followed by `::` prefixes the reference.
The check includes labels retained on directory rows and whole host labels
containing `::`. Revoked peers retain their host selectors. Historical labels
are not durably retained across re-enrollment: after old rows retire, use an
exact host UUID or explicit literal rather than relying on a former host label.
A legacy bare spelling is never
reinterpreted as host-qualified. A collision, unavailable peer, missing basis or
host-prefix match refuses with explicit `name:` and `host:` alternatives and an
exact-id option. Non-domain local session names keep their existing grammar.

The domain MCP session list returns a usable `name`, alongside `sessionId`,
`localSessionId`, `localName`, `hostName` and `hostId`. `sessionId` is the canonical
domain principal; `localSessionId` is its host's native id. Central-host listings
use `host:<host UUID>::<native session-id>`. On a satellite, a local row uses its
bare native id and the configured central uses the explicit host/native form.
Rows on other satellites use the canonical domain id: there is no direct native
relay pair between those hosts. A native id outside the copyable syntax also
uses the canonical id. Friendly names remain in `hostName`/`localName`.
If a satellite cannot reach its authenticated central directory, its fallback lists
only local native roots, with bare ids in `name` and friendly labels in
`localName`; unavailable inventory still refuses global aliases.
Ordinary native mail and its wake use the enrolled, non-revoked,
non-quarantined pair's current authenticated link. They require no verification
report, Active membership or per-session join. They do not activate the host or
grant executor placement.
Canonical ids retain domain delivery and its separate wake-authority contract;
mailbox-only targets can be delivered without an executor wake. A manually
supplied host/native address still requires its enrolled direct pair and is
refused when that pair is absent; it does not fall back to domain delivery.
Automatic directory discovery can admit eligible unbound roots for
routing; explicit native sends do not require that per-session admission.
Previously accepted bare `Host::Name` and `UUID::Name` qualified inputs must
now use `host:`; they receive actionable refusal instead of implicit routing.
For automated CLI or launcher `--notify` targets, use a bare native id for a
local root, or `host:<host UUID>::<native session id>` for a remote root.
Domain operations may use an exact domain principal or an explicit `name:`/`host:` alias. A bare literal containing
`::` may be refused whenever a peer is unavailable, and an automated sender
cannot choose an alternative on the operator's behalf.

The existing directory RPC opts into an optional `aliasBasis` response through
the MAC-bound URI query `?aliasBasis=true`; its JSON operation body stays
`{"operation":"directory"}`. Without the query, the response has exactly the
old shape. With the same query, the local participants API exposes
`{participants, unavailableHosts, aliasBasis}` from one automatic-discovery
snapshot; its default response remains an array. A new client talking
to an old host treats the absent basis as incomplete for legacy bare names
containing `::`, with no guess or cached fallback. Explicit `name:` literals
can still select a current row when no hosts are reported unavailable.
Exact-id resolution performs no additional discovery hop. This is a
discovery projection of the existing registry, not authority or a retained
lease; append still checks current participant and placement identity. The
federation profile contract stays `2`, with no storage migration.

`GET /api/sessions/{id}/mailboxes/participants` reads the current central
participant directory. `POST /api/sessions/{id}/mailboxes/create`, with
`targetSessionId`, gets or creates the central pair mailbox, including an empty
one. Session summaries expose optional `mailboxParticipantId` for explicitly
bound native sessions. Ordinary mailbox routes use that binding automatically;
a disabled or stale binding refuses access rather than reverting to local mail.

`GET /api/sessions/{id}/mailboxes/protocol` reports the central identity,
versions and limits: 256 KiB bodies, 4 KiB per metadata field, sanitized
160-character names, at most 200 messages per page, a 2 MiB encoded request
and a 320 MiB encoded response ceiling. The last ceiling covers the existing
maximum page with worst-case JSON escaping; it does not increase body or page
limits. Link negotiation and health keep their smaller transport bounds.
At four in-flight mailbox calls, this permits up to 1.25 GiB of encoded
response buffers per peer, before JSON decoding and stored response values.
The accumulator checks escaped wire bytes before extending and caps its
reservation growth at the negotiated bound. This is an explicit mailbox
resource allowance, separate from L1's small negotiation and health messages.

Authenticated mailbox requests retain the existing five-second listener
deadline. A deadline returns retryable `503` without invalidating the peer link;
the outcome may be unknown, so mutation retries keep the original idempotency
key. A canceled request's blocking storage work retains its admitted RPC slot
until it settles. Link negotiation and health retain their liveness timeout
and exact-incarnation invalidation behavior. A health probe refused with `429`
reports admission pressure: both monitors retain the current link without
recording a successful probe or resetting reconnect backoff.

A central deadline, lost transport response or unusable success response may
follow a committed append. The satellite preserves an explicit unknown-outcome
signal through the local API and bridge. Retry the original request with the
same idempotency key; an unavailable response never proves that C did not commit.

Mailbox domain wire/schema **2/2** adds authenticated, bounded error classes and
frozen read pages. The global federation transport/MAC version remains 1; primary
and coordination SQLite schemas are unchanged. Both hosts must run the new mailbox
contract and re-verify their pair before using it. A mixed mailbox version returns
`upgradeRequired`; it cannot downgrade to a preview, numeric ACK or local replica.
An authenticated plain 400 or 422 lacking the required v2 envelope is a protocol
refusal and returns typed `upgradeRequired`. A current peer's generic 400 uses a
correlated `request_refused` envelope instead; it grants no safe mutation retry.
Only a verified `writer_admission_timeout` proves that the requested mailbox write
did not commit. `directory_fence_unavailable` proves that the requested mailbox
operation was not entered, while the directory mutation may already have committed.
These two allow one exact retry inside the existing client budget. Handler deadlines,
transport loss and authority changes remain unknown outcomes; actual authority drift
requires reconciliation. Raw remote database errors are never forwarded.

A native receipt read captures its page through a separate read-only connection,
without waiting for coordination writer admission or its long-lived connection mutex.
The complete page, cursor and wake metadata are frozen under a process-scoped
`readKey`. Receipt issuance has its own two-second allowance inside the unchanged
five-second central request budget. A blocked issuance returns the page with
`receiptState: "receiptPending"`, its key and `receipt: null`. This is readable
content, never ACK, numeric-ACK, noContinuation or wake-settlement authority.
Only a real committed receipt can acknowledge it.

The existing native API, MCP and CLI read flow owns key creation and performs at
most one pending-page resolution inside its existing request budget. An identical
ordinary read can finish the one retained pending page; agents need no new command.
The native API retains at most 16 unresolved read intents (key, exact inputs and
binding only), so an explicit retry after a transient 503 resolves the original key
even across separate CLI invocations. This is memory-only client bookkeeping, with
no message cache, durable journal or automatic retry of an uncertain mutation.
The central protocol response offers the bridge a one-use admission key. An
unconsumed offer grants no read or ACK authority. The first keyed read consumes
it before capture. Both resolution flags expire when an accepted key is missing;
neither can recapture later rows after reclamation. The bridge manages this
without new agent steps. A full ring of pending pages offers no new key.
An explicit resolution preserves the original inputs, including omitted versus
explicit `afterSequence`. It adds issuance state and the receipt without selecting
later messages or replacing frozen cursor/wake metadata. `pageId` identifies the
versioned frozen descriptor; `cursorGeneration` binds the qualified participant and
its processed-through snapshot. Empty pages stay empty under the same key.

The ring retains at most 16 offers and pages combined, each page with a two-MiB
serialized bound, for at most ten minutes from its offer. Pages contain a nonempty
prefix of complete messages when more
rows fit on subsequent pages. Capacity is reserved before accepting a new page;
unresolved pages are never evicted to admit another. Unconsumed offers, issued
pages and empty/preview entries are reclaimable at capacity, so completed reads
cannot block later readers. Restart, expiry or a missing retained entry yields
typed `readKeyExpired`; a fresh logical read needs a new offered key.
There is no durable read journal or descriptor-based reconstruction after restart.

Domain sends first persist a sender-local delivery intent. They return a send
receipt only after the central append is confirmed and its outcome is retained
locally. The central append, canonical digest outcome and durable wake obligation
commit in one SQLite transaction. The immutable domain append receipt reports
`durableButNotWoken` at that commit point, for both remote and central native
recipients. It does not wait for notification or dispatch finalization; an exact
duplicate returns the same committed message identity without waiting either.
Later wake ownership is shown separately in message details. For a remote
recipient, central's mirror records only that the receiver durably owns the
wake; that mirror and the append receipt never change. Central then learns what
the receiver did with it: the wake drain owner asks the receiver, over the
authenticated pair (`POST /federation/v1/executor/outcome`, with the exact
operation identity and payload digest), and the receiver answers read-only from
its own journal and the root's queue and delivery state: received, queued, queued
behind an active turn, held behind a paused queue, no longer queued with its state
unknown, woken (its runtime accepted this wake), rejected, handed off with the
outcome unconfirmed, or the mail processed without this wake (another turn's
continuation handled it, and this wake, once queued, is no longer queued or
handing off; a wake not yet queued, or still queued, after the mail's ACK is
answered by its queue state, since it may yet run). Only positive runtime acceptance is woken. The receiver
answers only its configured central over the current lease, and only for an
operation whose journal identity, digest and target binding are current; any
other query is refused. Central creates the wake's outcome row when it settles
and keeps the answer there (`federation_executor_outcomes`), under the same
authority fences as the mirror; an answer those fences refuse is dropped, but the
attempt and its backoff still count. A terminal answer (woken, rejected,
unconfirmed, processed without this wake) is final and ends polling; other
answers are polled again with backoff, at most a bounded number of times and
within a bounded age from settlement, only on reachable peers. Polling runs one
pass at a time in its own task, started by the drain owner's fallback tick and
never on the wake dispatch path, so a slow receiver answer cannot delay wake
dispatch; a pass starts no new poll after its time budget, and each poll uses
one of the peer's RPC slots under the transport's own bound. A receiver whose
answer misses its handler deadline returns an authenticated retryable refusal,
which counts only as an attempt; unlike other executor requests, it never
fences the pair's link. Message details show a terminal
answer as the outcome, a non-terminal one as "wake outcome pending; last seen:
... at T", no answer as "wake outcome pending", and add "no longer polled" once
either bound has ended polling; never "not woken". For a central native
recipient, an owned worker in the federation supervisor reads the committed
obligation, revalidates the exact target binding while queuing, and uses ordinary
queue coalescing and dispatch. It is kicked after commitment, falls back to a
bounded recovery scan, and is joined at shutdown. A lost kick or HTTP reply does
not discard the obligation. Until it is queued, message details report
`committed; native wake pending`. Standalone mailbox sends retain their existing
receipt and notification behavior.

The negotiated `senderIntentRegistry` flag describes the central host, including
when a satellite forwards its protocol response. An authenticated central that
advertises `false` causes a visible `409` refusal before a new intent consumes
quota. Transport unavailability can still admit bounded sender-local queued
intents under the existing exact native creation, binding and domain authority,
even without an earlier successful negotiation. This is `localIntentDurable`,
never a receipt or an append attempt. Stale, revoked or unauthenticated authority
is not treated as an outage. Exact send and cancellation dispatch always require
affirmative capability evidence fenced to the actual current authenticated link;
there is no fallback to the older send operation. A reconnect requires fresh
negotiation. Upgrade the central before using durable sender intents.

If a never-attempted queue later meets a central without this capability, a
writer-serialized local refusal releases its pending quota and preserves the
terminal key and digest. Its status is `refused`, with `refusalSource: "sender"`
and `refusalReason: "centralCapabilityUnsupported"`; this is not a central-issued
receipt or tombstone. An already `appendOutcomeUnknown` intent stays unresolved
and keeps its quota: a later capability refusal cannot prove non-commit.

The sender registry is a retry outbox, never a mailbox replica. A pending
`localIntentDurable` or `appendOutcomeUnknown` row has no canonical message id,
sequence, read receipt or mailbox cursor. Its bound is global to the sender
host: at most 1,000 pending intents and 64 MiB of encoded retry payload across
all sessions and targets. Exhaustion refuses a new intent without evicting an
ambiguous outcome. The key namespace includes the qualified sender creation
and authority epoch and spans every target. The versioned digest binds exact
sender and recipient bindings, the key, UTF-8 body and optional metadata;
display aliases and link incarnations do not change it.

Sender-local API routes are `POST /api/sessions/{id}/mailbox-intents` to prepare
without sending, `GET /api/sessions/{id}/mailbox-intents/{key}` for status, and
`POST` on that key's `/retry`, `/cancel` and `/gc` routes. Status belongs to the
native root session's exact creation and stays locally readable while its
federation authority is held. The prepare request may include an optional full
`targetBinding` descriptor with
the ordinary message fields. Its `targetSessionId` must equal that binding's
canonical participant key and its central identity and authority epoch must match
the server-derived sender binding. This permits a first offline prepare without
directory discovery; the descriptor is caller-supplied routing input, not proof
the recipient exists. With the field omitted, a fresh target still requires
central directory resolution. Ordinary send rejects `targetBinding`; the field
belongs only to preparation. It never installs a binding or creates directory
authority, and C validates the captured exact recipient at dispatch. A retry
replays the captured binding and bytes and never retargets an alias.
Central first replays an existing committed,
refused or abandoned outcome under current sender authority, even if the
recipient has since retired. A temporary recipient link or authority hold
is not a permanent refusal: only definitive native-creation retirement or
the exact completed remote placement can be fenced through a new refusal
commit. If local outcome retention fails after a confirmed append, the error
states that C committed and asks for an exact retry to recover that receipt.
Canonical ids take precedence over display aliases; missing canonical ids
and bare native ids return `404` without alias fallback on satellites too.
A never-attempted intent can be cancelled only by
a serialized central tombstone. Possibly sent intents visibly refuse
cancellation; a negative lookup cannot prove that a delayed first send is
impossible. Terminal `homeCommitted`, `refused` or `abandoned` rows may discard
retry payload while retaining identity, digest, target and outcome. Namespace
retirement and central tombstones have no time-based expiry. These routes are
disabled with the M1 federation setting. Standalone mailbox behavior is unchanged.

Coordination schema version 5 upgrades exact versions 1 through 4 atomically,
preserving mailbox bodies, cursors, issued read receipts, delivery intents and
executor journal records. Version 4 introduced the executor journal; the
authenticated ordinary wake receiver and bounded recovery use that existing
layout without a further migration. Version 5 adds standalone continuation
audits, never recovered runtime coverage.
The migration is one-way: an older binary without version-5 support visibly
refuses to open the upgraded coordination.sqlite, preserving its mailbox data,
receipts and cursors. Reverting only the binary is unsupported. There is no
automatic backup, downgrade or reset. Coordination's 4-to-5 migration is separate
from primary `termal.sqlite` versioning. Unknown or
modified schema objects still refuse opening; no mailbox data is inferred from
intents. The unlanded audit-only version-4 layout is not a supported production
migration source and is refused without rewriting it.

For an already placed native session, C sends only wake metadata through the
authenticated executor endpoint. S commits the captured operation and payload
digest before returning durable ownership. An exact repeat reuses its operation
and queue identity, including after reconnect; changed content under its key
conflicts. No mailbox body or workspace crosses this channel, and it installs
no session binding or Start/Stop authority.

The operation journal and session queue are separate durability surfaces. A held
queue entry, full operation association and PromptDelivery nonce must be read
back from the same session row before dispatch. Provider handoff retains one
logical effect identity and separately fences each local write-ahead nonce.
Positive runtime evidence and a validated central mailbox ACK advance separate
states: an ACK cannot clear an unknown runtime effect. Recovery materializes
missing queue associations, reconciles positive evidence, and holds uncertain
handoffs without replay. It never resumes a dormant queue or bypasses explicit
pause/Resume. The supervisor also retries retained sender intents using their
original envelope, native owner and idempotency key.

C settles a pending wake and inserts its matching receiver-ownership mirror in
one coordination transaction under current-peer and exact-binding guards. A
mirror is identified by `targetHostId` differing from the local host identity,
regardless of configured role. Its columns stay `received`/`not_started`, with
no queue, runtime or processed evidence; it can never execute locally. Legacy
settled obligations without mirrors mean unknown receiver state and never replay.

Before dispatch, the existing journal rows and an immutable-order prefix of
pending remote wakes reserve capacity. Appends extend the pending suffix;
settlement exchanges a reserved slot for equal mirror payload bytes without a
quota recheck. Beyond-prefix wakes remain pending with reason `capacity`, while
their original append receipts remain valid. There is no eviction. Until bounded
retirement of resolved evidence is implemented, these are lifetime limits of
65,536 retained operations and 64 MiB of payload per host, with 256 KiB maximum
per operation. They must not decrease without a recorded migration decision.

The dispatch window is separately limited to 1000 eligible obligations, ordered
by immutable rowid. A process-local cache scans pages of 100 raw rows, at most
1000 per pass, and keeps its scan position across passes. A deterministic
unsupported envelope or permanently stale placement is excluded individually,
shown as `unsupported` or `stale_authority`; I/O failure excludes nothing. The
cache rebuilds from durable rows when durable identity, placement or local role
changes, and on process start. Link disconnects never retire authority. A
previously dispatched matching receipt can still settle unconditionally even
if a later build makes more obligations eligible and the cap is temporarily
exceeded; new dispatch waits at the cap.

Wake envelopes keep mailbox, message and sender identifiers exact. Only display
copies of sender name and topic strip control characters and truncate at UTF-8
character boundaries (512 and 2048 bytes respectively); original metadata stays
at C and remains readable. The normalized envelope determines the digest.
Append commits and link reconnects kick one drain owner. Each batch attempts at
most 16 wakes and stops after its current bounded RPC when its two-second work
budget expires; peers without a lease are skipped. The periodic sweep is a
fallback, and unchanged queue/evidence recovery performs no writes. Before any
handoff, the exact current receipt/operation association is proved durable and
the journal's Queued association is repaired before EffectAttempt.

## Model in one paragraph

One explicitly configured **central TermAl host** is the control plane for a
coordination domain: it is authoritative for the domain's session identity,
placement and lifecycle admission, and it is the only home of every mailbox in
the domain. Any number of **satellite hosts** — macOS, Linux or Windows — are
enrolled with it. Satellites run their sessions natively and take part in
coordination through the central host's API, carried directly machine to
machine over OpenSSH. The topology is hub and spoke: there is no
satellite-to-satellite link, no mailbox replica and no storage replication
anywhere in the domain. Engram is not part of this control plane: it stays
decentralized, and its tasks meet through the shared Git remote.

## Terms

- **Host.** One TermAl installation with its own `termal.sqlite`,
  `coordination.sqlite`, sessions and coordinator. A host is an installation
  and its store, not a process: a boot is one generation of it.
- **Central TermAl host** (C). The hub for sessions and mailboxes, one per
  coordination domain. It is configured explicitly; it is never a hard-coded
  hostname, and the design is not tied to one platform.
- **Satellite host.** A spoke that runs native sessions and reaches mailboxes
  and coordinated session control only through C's API.
- **Coordination domain.** One central host and the satellites enrolled with
  it.
- **Standalone host.** An installation outside any domain. It keeps today's
  local behavior, and joins a domain only by explicit enrollment; nothing ever
  adopts its sessions silently.
- **Qualified identity.** Participants are addressed as `host_id/session_id`
  and named `Host::Name`. Labels and names are display and discovery aliases;
  routing always uses exact qualified ids, and an ambiguous name at send time
  is an error.
- **Shared Git remote.** Engram's meeting point for task planning; it is not an
  authority, and it is not part of this control plane.

## Local operator configuration

`GET /api/federation` reports the current `enabled`, `role`, `local` and
`central` values, the domain `version`, `trackingDurability`,
continuity/quarantine status, the peer registry with declared and allocated
capacity, and, on a satellite, a `satellite` block (below). Host identities use
`{hostId, hostGeneration}`. Reading does not enable federation, create peers,
release reservations or change the store. This is a local operator API, not a
domain transport or satellite enrollment endpoint.

The [journal diagnostic](federation-journal.md) reads bounded executor history
and central wake obligations on this same local operator surface, with no
payload or evidence values and no configuration or dispatch side effect.

To enable this host as central, first read its identity, then send
`POST /api/federation/settings` with:

```json
{
  "enabled": true,
  "role": "central",
  "central": {"hostId": "<local.hostId>", "hostGeneration": 1}
}
```

Copy both identity fields from the current read; the example generation is not
a constant. The setter requires `enabled` and `role`, rejects unknown fields,
and validates the complete central identity against this host. It calls the
existing domain configuration path: the mutation advances the domain version,
invalidates old link verification when settings change, and confirms the exact
content through the state writer before returning success. Its JSON response
is the current registry view. Verify `enabled: true`, `role: "central"`, the
matching `local`/`central` identities, and `trackingDurability: "durable"`.
The setting persists across restart; enabling never mints a new host identity.

Disable through the same POST with `enabled: false`, retaining `role:
"central"` and the same `central` identity. This holds domain admission and
invalidates links without deleting peers, existing local sessions, mailbox
history or unresolved reservations. Returning to `role: "standalone"` requires
`enabled: false, central: null`; M1 refuses role replacement while unresolved
placements remain.

### Configuring a satellite

To make this host a satellite of a named central, read the central's own
`GET /api/federation` and copy its `local` identity, read this host's
`version`, and send to this host:

```json
{
  "enabled": true,
  "role": "satellite",
  "central": {"hostId": "<central's local.hostId>", "hostGeneration": 3},
  "expectedVersion": 12
}
```

The same setter and domain path apply: the central identity is validated (a
canonical host id that is not this host's, a generation of at least 1), the
change is fenced on `expectedVersion` (required for any change from or to the
satellite role, and honoured whenever it is given, so a central client of M1b
keeps working without it), and the exact content is confirmed through the
state writer before success. Verify `role: "satellite"`, the bound `central`,
the advanced `version` and `trackingDurability: "durable"`. The binding
persists across restart and never mints a new local identity.

Disable with `enabled: false`, keeping `role: "satellite"` and the same
`central`: it holds domain admission and invalidates the link, and keeps the
central binding, its peer record, local sessions, mailbox history and domain
mailbox bindings. Returning to standalone is a separate request,
`{"enabled": false, "role": "standalone", "central": null, "expectedVersion":
…}`. Local sessions and mailboxes without a domain binding behave the same in
every state.

The `satellite` block of the read view shows what an operator checks before
the central runs [O1](satellite-onboarding.md): the bound `central`;
`centralPeer` with `enrolled`, `membership`, `recordGeneration`,
`transportGeneration`, `verificationCurrent`, `lastObserved` and
`linkConnected`, which is true only while federation is enabled, durable,
neither quarantined nor continuity-lost, and holds the current link to that
central (never a past observation); and
`boundSessions`, the local sessions holding a domain mailbox binding. It shows
credential references only, never key material.

A typed refusal carries a stable `code` beside `error`:

| Status | `code` | Meaning |
| --- | --- | --- |
| 400 | `federation_expected_version_required` | A satellite change names no `expectedVersion`. |
| 409 | `federation_version_conflict` | The settings changed since the version read. The version also moves on every enrollment, connect and probe, so read it immediately before posting. |
| 400 | `federation_invalid_binding` | The role and central do not fit, for example a central that is this host. |
| 409 | `federation_central_stale` | An older generation of a central this host already knows: the bound one, or a retained peer record of it, revoked included. |
| 409 | `federation_central_mismatch` | Another central, or a new generation of it, while a peer is enrolled under the old binding: revoke it first. |
| 409 | `federation_bound_sessions` | On a satellite, local sessions hold a domain mailbox binding to the current central: the role or central cannot be replaced until those sessions are deleted or retired. No unbind workflow exists yet. |

Malformed JSON, unknown fields and missing required fields return 422;
well-formed invalid settings return 400. Existing authority/placement conflicts
return 409. A 500 persistence failure is an unresolved outcome: the attempted
settings may already be visible in memory, but admission remains held and the
read view reports `trackingDurability: "unresolved"`. Do not interpret that
view as a durable enable or assume the failed request rolled back. Resolve
the persistence failure through the existing durability recovery path before
another configuration attempt. Never edit the live database as a workaround.

## Topology: hub and spoke

- One central host, N satellites. Nothing fixes the number of satellites:
  adding another satellite of an already supported platform is configuration
  plus a successful onboarding verification, never a code change. A platform
  that is not yet supported needs its own platform lane first (see the
  rollout).
- No satellite mesh: satellites never link to, route through or own anything on
  each other. A conversation between two satellites goes through C.
- No storage replication: no mailbox is ever duplicated, mirrored or merged,
  and no host keeps a copy of another host's store.
- Satellites are independent machines and execution environments, not
  independent authorities. There is never a second authority for the same
  session or mailbox, no election and no timeout-based failover. Changing which
  host is central is an explicit operator migration, outside this design.
- Executor proxies stay a different kind. A project bound to an SSH remote
  (see [Project-scoped remotes](project-scoped-remotes.md)) keeps routing as it
  does today; enrolling a host in a domain never converts an executor-proxy
  session, and an SSH executor binding never proves domain membership.

## Protocol: the TermAl API over OpenSSH

The application protocol between hosts is the TermAl HTTP API. It runs
directly machine to machine, never through the UI, a browser or a human relay:

- C opens one OpenSSH session to each satellite (batch mode, verified host
  key, keepalive). It carries HTTP both ways: a local forward for C's calls to
  the satellite and a reverse forward for the satellite's calls to C, both
  bound to loopback.
- The forwarded listener serves only the domain routes, never the general
  local API. Every request carries the pair capability minted at enrollment;
  the receiving host identifies the caller by that capability and that
  channel, never by a host id written in the payload.
- C calls a satellite only to deliver wakes, check liveness and run the
  coordinated session commands below. Everything else flows from the
  satellite to C.
- Per link: at most four RPCs in flight, exponential backoff with jitter from
  one second to five minutes, no tight polling, and no network I/O while the
  global state lock is held.
- Protocol version, schema and size limits are negotiated explicitly. Today's
  mailbox limits apply unchanged (body 256 KiB, topic and `stateStamp` 4 KiB
  each, display names 160 characters).
- Mutual TLS between hosts and per-request signing are later slices; the
  pilot authenticates with the pair capability over OpenSSH.

An agent's MCP bridge and CLI keep talking to their own host. That host serves
the call itself when it is C, and relays it to C otherwise; a relay route holds
no mailbox state.

## Mailboxes: one canonical store at the central host

### Automatic native participation

Enrollment and the existing current authenticated pair proof qualify a host
for mailbox participation. Central native root sessions and enrolled satellite
native root sessions appear in the domain directory automatically, including
sessions created before enrollment or before their first send. No explicit join,
fresh session, per-session authorization, or executor activation is required.
The MCP bridge discovers the domain through the caller's own host.

Automatic bindings are mailbox-only. They reserve no executor session, build or
gate capacity and do not promote membership to Active. Execution still
requires explicit Active admission and current verification; an operator placement upgrades a mailbox-only
binding while preserving its creation identity and placement generation. Hidden
sessions, delegation children and executor proxies are not enrolled as native
domain principals by discovery.

The satellite persists automatic routing separately from its explicit placement
binding (`federationAutomaticMailboxBinding`). The executor receiver, recovery
and provider handoff continue to require the explicit binding; a signed central
wake alone cannot upgrade automatic participation. An explicit Active placement
moves the same identity to the explicit binding in one durable transaction.

Discovery uses bounded cursor steps over the authenticated channel. A cold child
index examines at most 64 delegation links per step; routine delegation status
updates do not invalidate it. Catalog pages scan at most 64 session records and
return at most 32 candidates. Each discovery step admits at most one new root,
skips already registered bindings in batches and never reinstalls a current
binding. Satellites drive separate steps instead of nesting a domain sweep
inside one request. A fenced root is omitted without hiding other roots.

The directory endpoint (`/api/sessions/{id}/mailboxes/directory`) and MCP session
listing return `unavailableHosts` beside healthy participants. An unsupported or
unavailable peer is omitted and identified by host, reason and status. The older
`participants` endpoint retains its array shape. When hosts are reported
unavailable, global display-name aliases, including explicit `name:` literals,
are refused; use an exact qualified id or an explicit `host:` target on an
available host. With an empty `unavailableHosts` list, an explicit `name:`
literal can select a current row even if `aliasBasis` is absent or incomplete.
Legacy bare names containing `::` additionally require a complete basis and
must not begin with a known host selector.
Availability checks use the shared alias parser and the same snapshot's host
basis, rather than interpreting `::` separately. An exact
`host:<host UUID>::<native session-id>` still requires a current directory row;
it does not admit an absent root. Enrolled peers may participate in mailbox
discovery without gaining executor authority. Alias resolution refreshes candidates before
deciding uniqueness, including roots created since the previous directory read.
If a satellite cannot reach its domain directory, MCP session listing still
returns the host's local native roots and identifies the unavailable central.
Their bare ids remain usable locally; incomplete global names remain refused.
Local authority loss still refuses the operation. A failed domain operation is
never retried against a local mailbox store. Draining peers retain existing
mailbox authority but gain no new automatic bindings. Ordinary sends and lists
revalidate automatic satellite admission against the current enrolled pair;
only obsolete automatic routing may be superseded, never an explicit grant.

Mail delivered before an explicit executor grant remains mailbox data. Its wake
obligation is settled in the append transaction and cannot become executable
work when that root later receives an explicit placement. New mail after an
explicit grant retains normal executor wake eligibility.

Pre-existing standalone history stays in its original store with the original
mailbox/message ids, sequence, cursors and issued receipts. Lists expose that
history alongside domain mail; reads and ACKs select the original native owner
before domain routing. New domain conversations use qualified participant ids;
there is no copying or merging of historical mailbox contents. A bare native
session id continues to locate a session on the caller's own host, never a
same-named session on another host. Remote targets use the current domain
directory's qualified ids or unambiguous host-qualified names.

A bare native session id selects a local-to-local send in the caller's own
store, even when either root already participates in the domain. It does not
require a central connection or host verification and does not enroll either
root. Domain targets still require participation; their refusal never falls
back to a local append. Existing native receipts, relay pins and domain intents
keep their original sender/key ownership and outcome when retried. Local and
domain conversations remain separate, with no copying or merging of mail.
Native reads, ACKs and wake recovery retain the native mailbox owner.
The domain-only create and intent-preparation endpoints refuse bare native
targets before participation; use a qualified domain participant or domain
alias there. An ordinary send remains the entry point for local native mail.
An intent key remains owned by its native session and creation across automatic
route supersession. A retry under a replacement sender cannot admit another
intent; it reports the retained prior sender and preserves the original status
and outcome without granting access to the old domain principal.

Automatic participation currently also retains the existing role and central
replacement guards. Once roots have joined, replacing the role or central can
be refused even though those roots hold no execution grant. Disabling and
re-enabling the same domain preserves stored history and receipts. Ordinary
reconnects retain participant identities, conversations and native creation ids;
old leases remain fenced. If re-enrollment supersedes an automatic satellite route, that native root retains
the old route descriptor. Mailbox listings expose `unavailablePriorDomains`
(also returned by the MCP mailbox list) with the prior participant id, central
and reason. The replacement identity cannot read or ACK as the retired identity;
old messages and issued receipts remain in their original store, unavailable
rather than silently omitted. The array API exposes the count through
`x-termal-unavailable-prior-domains` and links to the availability response. There is
no automatic unbind or routing-retirement workflow in this change; explicit
execution bindings retain their replacement fences as well. Safe retirement on
an explicit operator replacement is separate work, requiring old-domain
unavailability to remain visible without dropping or rerouting retained mail.

### Downgrade boundary for automatic participation

An older satellite ignores the new optional automatic-routing field and sees no
explicit execution binding for those roots. It cannot use their automatic domain
participation, and cannot interpret it as an execution grant. Existing explicit
bindings remain unchanged. No SQLite schema floor changes in this feature.

Central rollback below this feature is unsupported once a mailbox-only placement
has been persisted, including a completed placement retained as retirement
evidence. Older `FederationPlacement` readers reject its `mailboxOnly` field
(`deny_unknown_fields`). At base `cde926c4`, metadata decoding propagates that
error through `load_state_for_boot` and host construction before startup; it
does not drop the placement or create a grant. Roll forward to a supporting
binary to recover. A rollback instead requires the operator to restore a
complete pre-feature state backup through the existing restore procedure before
starting the older central; disabling federation or merely deleting a session
does not remove this persisted format boundary. No automatic downgrade rewrite
or direct-store editing procedure is provided.

Every operation of today's mailbox contract
([Durable agent mailboxes](agent-mailboxes.md)) keeps its meaning, operation
for operation: get-or-create, send, list, read a page (C issues the receipt),
read an exact message, acknowledge, plus a C-initiated wake to the
participant's host that raises the ordinary session wake (metadata only, never
the body).

- **Storage exists only at C.** A satellite serves list, read, read-message and
  acknowledgement only by calling C. There is no local fallback, no cache
  serving, no offline read and no local SQLite replica of any mailbox.
- **What a satellite keeps.** Per mailbox and local participant, routing
  fields only: C's host id and authority epoch and the qualified participant
  id. It holds no sequence, cursor, message or receipt, and nothing it keeps is
  ever returned as mailbox content.
- **Receipts and cursors.** A page receipt is issued only by C and is bound to
  the mailbox, the participant's host, the participant, C's authority epoch
  and the issued range, so a receipt from another host or epoch is refused.
  Acknowledgement is a compare-and-swap at C with the unchanged receipt; no
  numeric boundaries cross hosts. A later receipt cannot skip an earlier
  unacknowledged page, and a send never leaps over unread inbound messages.
- **One mailbox per pair.** A canonical participant-pair key deduplicates
  get-or-create at C: two opposite first sends at the same moment still create
  exactly one mailbox and one membership record.
- **The sender outbox is retry state only.** Each sender host keeps a durable
  intent registry for its own unconfirmed sends, keyed by sender session and
  idempotency key across every target, so a reused key with a different
  target or body conflicts before any network call. The registry is written
  before the first RPC. It is never a copy of a mailbox, never listed as
  mailbox content and never consulted for list, read, acknowledgement or
  cursor. After a send is confirmed or terminally refused, its payload may be
  removed; only minimal key, digest, target and outcome metadata stays, for
  idempotency.
- **Exactly once where it can be.** C deduplicates by sender host, sender
  session and idempotency key with a digest of the content: the same key and
  digest return the original message (`duplicate=true`), and the same key with
  different content is a conflict. Append, dedup row and wake obligation commit
  in one transaction at C. Transport is at least once and the canonical append
  is exactly once; agent processing makes no exactly-once claim.
- **Intent states are shown as they are.** An outbox intent is queued,
  possibly sent (outcome unknown), committed (with C's message id and
  sequence), refused or abandoned (an explicit, locally authorized act whose
  outcome is preserved). A queued or unknown intent is never shown as a
  successful send receipt, and a client retrieves a pending intent by its key
  instead of creating a new one. At most 1,000 pending intents and 64 MiB per
  sender host; beyond that a send is refused visibly, never silently evicted.
- **Outcomes never expire; cancellation is serialized at C.** The outcome of
  every committed, refused, cancelled or abandoned intent (key, digest, target,
  final state) stays authoritative for the lifetime of the sender's namespace,
  until that namespace is retired whole (the session is deleted, or the host is
  re-enrolled under a new epoch), and the namespace is permanently fenced after
  retirement. Only retry payloads and diagnostics may be removed. Any
  cancellation needs a terminal tombstone for that key serialized at C, because
  a negative lookup at C is not proof that a send was never made; the first
  version may refuse to cancel possibly-sent intents. A cancelled intent ends in
  the abandoned state above and is shown as abandoned, with its outcome. A
  delayed first send after a cancellation can therefore never append a message
  that was believed cancelled.
- **Ordering.** One dense sequence per mailbox. Nothing is promised across
  mailboxes or hosts, and wall-clock time never orders or expires anything.

Three receipts stay distinct and none proves more than its own step: C's append
receipt proves a durable append; a satellite's wake acceptance proves durable
local wake admission; a processed-through receipt proves a cursor advanced over
a correctly issued page. None of them proves comprehension, task completion or
Engram verification.

## Identity, enrollment and authority

- **Host identity.** Each host has a `host_id` (a UUID minted once and kept in
  `termal.sqlite`), an enrollment epoch (for a satellite) or an authority epoch
  (for C), and a link incarnation per boot. An ordinary reboot changes only the
  incarnation. Re-enrollment, a key or capability change, or a restore or
  clone that cannot prove continuity changes the epoch; then every older
  in-flight operation, receipt and capability is refused, and old pending
  operations are quarantined for explicit reconciliation.
- **Enrollment** is an explicit operator act. It exchanges host ids, SSH host
  keys and the pair capability (a random secret bound to that satellite and C,
  kept in protected local credential storage on both).
- **Session identity.** For a coordinated session, C allocates a creation
  identity that is never reused and binds it to one enrolled satellite and one
  runtime placement. A session deleted and recreated under the same name
  inherits no cursor, unread mail, pending command or access.
- **Central session control.** C may create, prompt and stop sessions on a
  satellite through the enrolled link, as project-scoped remote proxying already
  does today. The satellite still checks its own local safety and admission
  rules before applying a command, and keeps its process handles and journals as
  local operational state, never as a second coordination store. Restarting,
  installing or configuring a satellite host stays the operator's act. A local
  emergency stop is always available and grants no authority to start a
  replacement.
- **Authority.** A host acts only for its own sessions. The sender of a request
  is the authenticated peer (by capability and channel) plus the session that
  peer asserts, never a value written in the payload. Eligibility — a visible
  root session native to an enrolled host, not a delegation child — is judged
  by the session's own host and validated by C on every operation. Removing a
  participant or revoking a satellite fences future admission but cannot undo
  what was already accepted, cannot stop a disconnected process, and never
  rewrites history.
- **Fencing.** Every cross-host request carries the host id, the epoch and the
  link incarnation, and the receiving host validates them against its
  registry. An operation captures C's authority epoch, the satellite's
  enrollment epoch, the session's creation identity and placement generation,
  and the link incarnation before any off-lock transport, and validates them
  again adjacent to the central commit and again adjacent to the local runtime
  apply; a sender's preflight check alone is not enough. A stale one is refused
  with the same retryable `409` used for remote-lease ABA today, and is never
  retargeted to another host. A stale central epoch, a revoked or re-enrolled
  satellite, an address that changes and changes back, a recreated name, a
  replaced placement and a delayed response are all fenced cases.
- **Trust.** The pilot covers one operator's explicitly enrolled machines,
  with loopback-only forwards. Transport authentication proves which enrolled
  host sent a request, not that a reported result is true. Multi-user trust
  needs its own access-control and threat design.

## The satellite registry: membership and capacity

C keeps one registry entry per satellite: its host id and enrollment epoch,
platform (OS and architecture), transport endpoint, references to its pinned
host key and pair capability (never the secrets themselves), its verified
protocol and schema, diagnostic binary and toolchain observations,
declared limits (maximum concurrent sessions, build and gate slots) with current
allocations, its lifecycle state, and last-observed liveness. Protocol/schema,
host identity, enrollment and pair authority are checked on a fresh authenticated
link after reconnect. A hash-only rebuild or toolchain observation change never
requires a profile refresh or re-enrollment. The central monitor automatically
restores previously qualified membership and unfinished placements after fresh
bidirectional identity proof. It retains initial platform/wake evidence with
explicit provenance rather than creating another isolated wake. See
[satellite onboarding](satellite-onboarding.md) for the clearing events and
quarantine fence. Old transport leases never regain authority.

The execution profile schema is the independently versioned federation
contract `"2"`, not the local SQLite layout version. Upgrading local storage
preserves enrolled profile `"2"`; it neither refreshes the profile nor permits
unknown contract versions. Both hosts must run the compatible federation code
before their fresh authenticated reconnect can restore previous authority.

- Membership, state and placement changes serialize at C.
- A registry entry creates no executable session and grants no stop, start,
  configuration or approval power beyond the central session control described
  above.

| State | Meaning |
| --- | --- |
| Enrolled | Record created; not verified; no placement. |
| Verified | The onboarding checklist passed; still not accepting placement. |
| Active | Explicitly activated: link up, accepting new placement. |
| Draining | No new placement; already admitted sessions finish, report and reconcile. |
| Revoked | Future admission under the old identity refused; history and uncertain outcomes kept for reconciliation. |

- Only an Active satellite whose verification matches its current enrollment
  epoch, endpoint, host key, protocol and execution profile accepts new
  placement. A change to any of these removes eligibility until the satellite
  is verified again.
- Admission reserves capacity atomically with the placement, so two
  concurrent admissions cannot both take the last slot, and repeating an
  admission key recovers its original reservation. A reservation outlives an
  uncertain launch or stop and a disconnection until it is reconciled with its
  runtime owner; stale liveness or a timeout never releases it.
- Declared limits are ceilings, not grants: today's disk and gate constraints
  stay independent admission limits that registry capacity cannot override.
- The registry has no fixed roster and no hard-coded count. Choosing among
  eligible satellites, balancing and preemption are a later scheduler design
  that builds on this registry; this design defines none of it.

## Onboarding a satellite

Onboarding has two parts:

1. **Operator setup.** The operator's own acts on the satellite: an OpenSSH
   server, the account, the key, the TermAl binary. Then C's configuration of
   the satellite's registry entry.
2. **Automated verification.** C runs a checklist within that setup:
   reachability; host-key match; pair-capability exchange; compatible TermAl
   version and schema; qualified identity; authenticated RPC in both
   directions through the local and reverse forwards on the domain routes;
   wake delivery to an isolated probe session with its own recorded outcome;
   platform, path and process-supervisor probes in that session; and refusal on
   a mismatched identity, version or capability.

Reachability alone, or an SSH shell exiting with zero, proves nothing. A passed
checklist is bound to the full configuration and identity of the entry and
moves it to Verified; an incomplete or failed one leaves it Enrolled, and it
cannot initially become Active without operator activation. A material change to
that execution authority requires the checklist again. An ordinary reconnect
retains membership and uses fresh SSH/pair proof for mailbox routing, without
running requalification or fabricating a current verification report. The old
report remains diagnostic and `verificationCurrent` stays false on a new link
until an explicit verification succeeds. Execution placement keeps its separate
current-verification and incarnation fences.

Engram's own onboarding check (an Engram binary and automated Git exchange on
that platform) is a separately recorded step, run only for a host with Engram
sync enabled. It gates that host's Engram sync readiness, never TermAl
membership or activation: a TermAl-only satellite activates without it.

## When the central host is unreachable

- Work already admitted on a satellite may continue under its existing local
  rules, and the local emergency stop stays available.
- Central session control and authoritative mailbox list, read and
  acknowledgement are unavailable: they fail with a retryable `503`, and there
  is no offline read.
- No new central admission, no timeout reassignment, no alternate executor and
  no replacement session.
- A sender's own new sends may queue durably and are shown as outbox status,
  separately from mailbox history. They never get a substitute home, a
  fabricated sequence or a local success receipt.
- C shows a disconnected satellite's work as unknown or last observed, never as
  stopped merely because time passed. One satellite losing C affects only that
  satellite; the others and C continue.

On reconnection the satellite first authenticates the same C authority epoch,
reconciles its pending keys, and then replays its exact intents. A lost reply
after a commit returns the same result on retry; a failed wake after a commit
stays committed as `durableButNotWoken` and is restored by the recovery pass.
An unknown runtime effect stays unknown until it is resolved where it ran; a
negative lookup or a timeout never licenses a second start. Revoked or
epoch-mismatched traffic is held for explicit reconciliation, never readdressed.

Wake and command obligations on a satellite:

- The satellite persists a deduplicated wake or command obligation before it
  reports the obligation accepted. Received, queued, runtime-accepted and
  processed stay separate states, each visible.
- Duplicate transport events cannot create duplicate queue entries, and an
  acknowledgement preserves an already accepted continuation.
- Boot on either host recovers its intent and wake or command journals.
  Obligations have visible pending, delivered and abandoned states and bounded
  retry; an offline peer does not block unrelated local work.
- A disk-full or quota failure before local durability fails visibly, and a
  stale wake-state report cannot regress a later state.

## Observability

Every response, log line and UI row carries C's id and authority epoch, the
mailbox and message ids and sequence, the sender's intent key and digest, the
link incarnation and a trace id; transport errors disclose no credentials or
bodies by default. The
UI shows three things separately and never conflates them: whether C is
reachable and serving authoritative reads; the local runtime status of
admitted work (last observed by C); and the session's own outbox (pending
versus committed, wake queued, held or unreachable, recipient processed-through).
The registry view shows each satellite's state, platform, capacity in use and
last-observed liveness, with credentials redacted.

## Engram boundary

TermAl mailboxes carry ordinary coordination only; they carry no task-sync protocol.

This is the second of the four sentences of the boundary shared with Engram's
sync design, quoted whole:

> Git carries configured typed task planning documents and explicitly eligible
> shared notes. TermAl mailboxes carry ordinary coordination only; they carry no
> task-sync protocol. Local claims, grants, runtime state, delivery cursors,
> native verification/evaluation records and native completion seals are not
> imported by task sync. A mailbox receipt is not an import receipt or
> execution authority.

- TermAl owns mailbox addressing, authentication, delivery, retry and
  deduplication, wake eligibility and ordinary conversations. Its receipts
  describe mailbox behavior only.
- Engram is decentralized: each host has its own store, and task planning meets
  through the shared Git remote (push, fetch, merge) followed by Engram's own
  import on each host. Disconnected hosts may take up the same task in
  parallel; that is an accepted product choice.
- Nothing in this protocol carries Engram claims, grants, import receipts or
  completion seals, and no native evaluation, seal or check record crosses as
  local verification. Disclosed result text, opaque identifiers and evidence
  references may cross only as attributed observations. A message such as
  "pushed commit X" coordinates agents but has no task-sync meaning: it
  triggers no import and acknowledges no task application. A mailbox
  acknowledgement never settles Engram's import reconciliation or grants
  authority.
- Engram's origin identity is Engram's own; any association between an Engram
  origin and a TermAl host is local display configuration, grants no authority
  and never travels on the mailbox wire.
- The mailbox work and Engram's Git task sync proceed independently: neither
  depends on the other.

## Never crosses hosts as authority

SQLite stores or files; private scratch; credentials, SSH keys and coordinator
tokens; usable claim or grant tokens; retained prompts or session authority;
another host's execution or delivery cursors as a receiver checkpoint; native
evaluation, seal and check records as local verification; source-root bindings;
delegation children; Engram claims, grants, import receipts and completion
seals.

## Rollout: the slice map

Every code slice runs fail-first tests on one machine with two TermAl processes
and a fake link, plus the repository's focused tests, full gate and an
independent review pair. Enabling the feature on real machines and restarting a
host stay separate from accepting a slice.

| Slice | Delivers |
| --- | --- |
| D0 | This brief. |
| M1 | Central-domain identity and configuration, the satellite registry with its membership and capacity model, enrollment records and explicit session placement — feature disabled. |
| L1 | The host-to-central OpenSSH link: one C-initiated session per satellite with local and reverse forwards, the domain-only listener and pair-capability channel binding. |
| O1 | Onboarding: the operator's setup and C's automated verification checklist, moving an entry to Verified. |
| M2 | Central mailbox routing, directory and protocol conformance: every operation goes to C; opposite first sends make one mailbox; an outage never yields another owner or cached reads. |
| M3 | Atomic central append, dedup and wake obligation, plus the sender intent registry, proven across crash cuts, lost replies, conflicting retries, bounded queues and a cancellation followed by garbage collection and a delayed first send; authoritative outcomes survive restart for the namespace lifetime. |
| M4 | Satellite wake and command journals, receipt-only acknowledgement and recovery, including C or a satellite restarting at every persistence cut. |
| M5 | One central host with one, then two satellites: the same agent APIs on every host and one central authority, including satellite-to-satellite conversation through C. |
| P (platform lanes) | One lane per satellite platform — transport, process supervisor and paths — for macOS first, then Linux and Windows, each verified by the O1 checklist. A Windows satellite needs an OpenSSH server on that host. |
| M6 | A bounded operator pilot: the central host plus a Mac satellite onboarded by configuration and the checklist, with exact build identities, delivery, wake and processing verified separately, and a reconnect, restart and disable matrix that preserves uncertain operations. Disabling drains or freezes pending intents visibly and preserves logs, cursors and dedup; rollback never discards unknown schema or outbox data. |

M1's registry and O1 are platform-neutral prerequisites of M5 and M6; M6 depends
on the macOS lane. A satellite of a platform without a verified lane cannot
become Active, and a real pilot proves only the platform and build combination
it measured. Until M6 passes, the operator relay stays the fallback for
cross-host coordination.

## Open questions (defaults apply)

- **Peer authentication.** Default: the pair capability over OpenSSH;
  per-request signing with a host key pair is a later slice.
- **Central session control.** Default: C may create, prompt and stop sessions
  on satellites, as described above.
- **Satellite capacity.** The operator sets each satellite's declared limits at
  onboarding; the design fixes no number.
- **Cancellation state.** The design names cancellation among the terminal
  outcomes without listing it as its own intent state. Default: a cancelled
  intent is recorded and shown as abandoned, with its outcome preserved.

## Related

- [Satellite onboarding](satellite-onboarding.md): operator setup, the named
  verification checklist and the independent Engram readiness step.

- [Durable agent mailboxes](agent-mailboxes.md): today's mailbox contract,
  which every host keeps operation for operation.
- [Project-scoped remotes](project-scoped-remotes.md): the SSH executor-proxy
  routing that stays a distinct kind.
- [Engram host adapter](engram-host-adapter.md): each host's own local Engram
  integration.
