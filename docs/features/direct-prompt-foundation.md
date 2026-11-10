# Direct prompt foundation

This foundation does not enable remote prompt delivery. The production pair
listener and local API do not mount its policy or caller routes. A complete
delivery path must connect to the existing executor, primary prompt queue,
`PromptDelivery` and provider observations before those routes are enabled.
The existing mailbox and wake protocols retain their behavior.

## Permission and identity

Permission records belong to `FederationState` and use its existing durable
mutation fence. They require two independent operator decisions: a central
caller grant and a destination consent. Both name exact host generations,
native session creation identities and enrollment generation. An existing
placement, when selected, is also fenced by its generation. Nothing is granted
by default. A reference is a view of a grant UUID, not a capability or another
registry.

Reusing an operator creation key returns its original identity, including a
revoked identity. Exact replay is a read of committed local history: it does
not require a new expected version or remote consent proof, and does not bump
the federation version or rewrite the permission. New issuance still requires
the current version and all current consent and native-identity checks.
Replay cannot retarget or revive that permission. Receiver
observation of revocation is recorded separately from local revocation.
Permission retention is bounded to 1024 records and 4 MiB without eviction.

Authenticated policy observations use the existing pair MAC and current link
and session owners. The requesting host measures freshness with its own
monotonic deadline, starting before the RPC. The issuer checks a returned
proof's wall-clock expiry in its own clock domain. An observation is transient;
retaining one cannot substitute for current permission at a later owner cut.
Policy and operator route factories are exercised only in disposable fixtures
at this stage. They do not advertise a production delivery capability.

## Data and bounds

The intent digest covers exact decoded text and immutable operation identity.
It excludes transport incarnation, nonce and expiry so reconnecting cannot
change the operation or grant it a new permission basis. Text preserves UTF-8,
CR/LF, whitespace and Unicode exactly.

| Value | Bound |
| --- | ---: |
| Decoded text | 65,536 UTF-8 bytes |
| Encoded request | 524,288 bytes |
| Request metadata excluding the entire encoded text token | 32,768 bytes |
| Body-free response | 16,384 bytes |

Strict codecs reject invalid UTF-8, duplicate and unknown fields, missing
required nullable fields, noncanonical UUIDs and zero generations. Request
bounds are checked before allocating a transport counter. The MAC covers the
original transmitted bytes rather than a re-encoded interpretation.

Immutable source intents and receive records live under the existing
coordination writer. They retain uncertainty and exact identity across reopen;
loading, reading or deduplicating a row never schedules or resends it. Queue
acknowledgement data is only a projection of a primary queue fence, not another
queue or provider lifecycle. Its actual queue-owner adapter belongs to the
delivery integration.

Retained prompt rows have budgets separate from mailbox intents, executor
records and wake reservations. Disposable foundation tests use 1000 sender
rows / 64 MiB and 65,536 receiver rows / 64 MiB, including serialized identity,
intent and reserved bounded evidence. These are prototype bounds, not a
production retention decision. Exact fixture duplicates are resolved before
capacity checks. There is no TTL eviction of operation identities.

Production journal mutations explicitly refuse before writer admission or
authorization callbacks, including duplicate admission and evidence updates.
Only test builds contain the fixture write mode. Mounting routes alone cannot
enable journal writes. Delivery integration must first define compact retained
identity with the actual primary delivery owner: settled bodies and reserved
evidence need not remain forever, while original identity and digest must
continue to reject conflicting reuse. A timeout or queue acknowledgement alone
is insufficient proof that an operation is safe to compact.

## Reader compatibility

The MAIN reader floor adds `direct-prompt-1` after
`native-test-completion-1`; an already activated import-reconciliation store
keeps its import journal at the corresponding successor. Migration preserves
raw metadata, advances the header and metadata generation in one transaction,
and does not backfill permission or activate imports. Presence of a policy key,
even null, under a predecessor floor is refused before typed normalization.

The primary-receipt checkpoint then appends `direct-prompt-delivery-1`.
Both ordinary and already activated `direct-prompt-1` predecessors advance
atomically to their corresponding successor, preserving raw metadata and the
import activation. A `remotePrompt` receipt key under an older floor is refused
before typed loading. The existing primary receipt retains a body-free exact
identity through pruning and reload; its queue-content fence also checks that
identity and the write-ahead nonce. This association alone cannot schedule or
resume delivery. Production handoff remains held until current authority and
provider correlation are integrated, and compact retirement is still owed.

Coordination step `8-direct-prompt-1` extends the frozen
`7-review-custody-index-1` inventory with intent and receive tables. Inventory,
marker and retained rows are validated before use. Unsupported combinations
are refused without adoption. Both stores use their existing transactions;
this does not claim an atomic commit across the two databases.
