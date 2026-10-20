# Federation journal diagnostics

`GET /api/federation/journal` is a read-only local operator API on each host.
It is not mounted on the authenticated federation listener. Reading never
enables federation, dispatches work, settles an obligation, refreshes the
reservation scan, or creates a persisted object. See the
[control-plane configuration](central-control-plane.md#local-operator-configuration)
and the [API catalog](../architecture.md).

Use `stream=operations` (the default) or `stream=wakes`, with `limit` from 1
to 100 (default 100). The response has `stream`, `entries`, `nextCursor` and
`dispatchScan`. Follow a non-null `nextCursor` using the same stream and
`cursor` parameter; treat the token as opaque. There are no key, stage or
time filters. Invalid or unknown query fields return 400.

Pages use observed SQLite insertion order, newest rowid first, rather than
wall-clock time. Updates to an operation's stage do not change this order.
The first page pins a high-water identity; later appends are absent from that
continuation. Each page validates both the high-water and previous-page
anchors inside its read transaction. A missing or reused anchor, or changed
host/domain basis, returns 409 with `code: journal_restart_required` and
`restart: true`. Start a new first page rather than skipping the refusal.
Cursors are stateless and at most 2048 bytes; they grant no authority.

## Executor operations

Operation entries contain `operationKey`, `commandKind`, `stage`,
`effectState`, `enrollmentGeneration`, `targetBinding` and `mirror`.
`targetBinding` carries the central host identity, qualified session identity
and placement generation. `mirror` means the target host differs from this
host. Mirror presence is retained receiver ownership evidence; it does not
mean the central host executed the operation.

Three booleans report retained evidence presence: `queueEntryPresent`,
`deliveryAttemptPresent` and `runtimeAckPresent`. `processedEvidenceKind` is
null, `mailbox_ack`, or `command_outcome`. No evidence value, queue/attempt ID,
ACK text, processed evidence text, payload or payload digest is returned.
The reader projects presence in SQL without decoding those values.

These fields can support a final-row stage-progression witness together
with the owning hosts' existing durable queue, turn and issued-page ACK
records. A late poll is not a stage history. Record `effectState` as read:
processing evidence does not upgrade an unknown runtime effect. In
particular, `processed` alone does not imply runtime acceptance.

## Central wake obligations

The wake stream requires the local central role. Other roles receive 409
with `code: journal_role_not_central` and their role, rather than an empty
success. Each row contains `messageId`, durable `state` (`pending` or
`settled`), `targetBinding`, `mirrorPresent`, `classificationState` and
`exclusionReason`. Mirror lookup matches both central host and operation
key. Malformed binding text is not echoed; its projection is null.

For pending wakes, `reserved` identifies a candidate in the observed
reservation prefix. `excluded` has reason `capacity`, `stale_authority` or
`unsupported`. `unknown` covers unavailable or mismatched observations and
rows not reached by the scan; it is not a permanent exclusion. Settled
rows retain their durable state and mirror flag without claiming a current
reservation. Neither a reservation nor a mirror proves dispatch or runtime
acceptance. Bounded display metadata may be checked through the native wake
validator, but is never included in this response; message bodies are not
read.

`dispatchScan` is null before an observation exists, on noncentral hosts,
or when its durable domain basis is no longer current. Otherwise it copies
the producer's process-local `floorRowid`, `scannedLastPass`,
`windowOccupancy` and `excluded` counters (`staleAuthority`, `unsupported`).
These are volatile producer observations, not durable totals or timestamps.
The reader copies the cache before its database transaction and leaves it
unchanged. It reports no dispatched/in-flight state.

Unresolved durable identity returns 503 (`journal_identity_unavailable`).
A failed storage read returns 500 (`journal_read_failed`), never an empty
successful page. The route provides neither activation nor a new way to
read private corroborating evidence; an operator run sheet must name the
already approved owning-host read surfaces before a live proof.
