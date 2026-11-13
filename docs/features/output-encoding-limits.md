# Encoded response limits

HTTP response bodies and individual SSE frames have a 67,108,864-byte encoded
ceiling. Smaller route and protocol limits still apply. This bounds response
encoding; it does not truncate domain records, retained transcripts or commands.

JSON uses one bounded writer. It checks the next byte count before reserving or
appending, counts UTF-8 and JSON escapes, and abandons partial output on failure.
SSE producers reserve the complete event/data framing overhead from that same
budget. The connection's lifetime is not subject to a cumulative byte limit.

An oversized HTTP response returns status 500 with `code: output_limit_exceeded`,
`phase: response_encoding`, `limitBytes`, a fixed message and `mutationOutcome`.
The outcome is `committed` only when the owner knows the operation completed,
`accepted` for known acceptance/enqueue, `unknown` otherwise, and `not_applicable`
for reads. Encoding failure does not imply rollback. Clients must use existing
status, receipt or read reconciliation before deciding whether to retry a
mutation. Success status/header decorators cannot turn this error into success.
Authenticated federation responses normalize the final error before signing.
Smaller federation protocol allowances retain their existing telemetry refusal
and mailbox unknown-outcome handling.

A rejected state or delta publication immediately emits ordered loss, invalidates
the broadcaster's diff base and leaves alignment requests unanswered. A rejected
generation is never a successful base. Receivers perform the existing full-state
recovery and transcript invalidation/repair. If the initial or recovery state
cannot fit, the server sends one small `outputLimit` event and ends the stream.
Browser transports retain their last view as stale and stop polling, reconnecting
and recreating the stream until the user explicitly retries. Remote connections
retain their mirrors and expose a remote-specific stale status; only explicit
connection replacement (disable then enable) clears the terminal latch. An
in-flight read from before loss cannot certify continuity after it.
Bounded history reads share that stop and generation fence; canceled navigation
demands settle without applying their old response, including after Retry.
An individual session tail that exceeds the limit can be retried by explicitly
selecting or reopening that session; passive hydration keeps its stop marker.
Remote mutations are refused before sending while latched. If the latch arrives
after a request was issued, a failed mirror application reports an unknown
mutation outcome and does not authorize automatic replay.

`StateResponse.remoteTransportStatuses` (also in the state-diff header) reports
stale SSH mirrors: `remote_disabled` requires `enable_remote`,
`remote_resync_pending` requires `wait_for_full_state`, and
`output_limit_exceeded` requires `disable_then_enable_remote`. Pending status
also covers an enabled remote that has not yet connected or supplied a full cut.

Overflow diagnostics use fixed producer names, the configured limit, bounded
session context when available, and revision numbers. Counts are coalesced in
fixed storage. `attemptedAtLeastBytes` is a lower bound observed at early refusal,
not an estimate of the complete output. Payload contents are never logged.

## Producer inventory

| Surface | Enforcement |
| --- | --- |
| Generic `Json<T>` responses, including errors and mutations | Shared bounded writer; request extraction still delegates to Axum. |
| Manual state, session-tail and history JSON | Shared writer in the blocking response owner; revision/session diagnostics retained. |
| Other finite HTTP responses and static files | Response publication guard checks the body's own length or collects unknown-length streams through the bounded writer before publishing headers. It does not trust Content-Length. |
| HTTP status/header decoration | Internal output-failure marker restores the error envelope before compression or signing. |
| Federation responses | The lesser of the existing response allowance and 64 MiB; final replacement bytes/status are signed. Existing 32 KiB/64 KiB protocol limits stay smaller. |
| Initial and recovery state SSE | Shared writer including framing; terminal control and close if recovery cannot fit. |
| Worker and synchronous state/delta publication | Shared writer; immediate ordered loss on refusal. |
| Marked/unmarked full cuts, legacy pairs and stateDiff | Shared writer; rejected data does not advance a base or satisfy an alignment generation. |
| Workspace batches and covering invalidations | Shared writer including framing; loss is signaled rather than silently dropping publication. |
| Terminal output/completion/error SSE | Shared writer; bounded command error, with no automatic command replay. Existing command capture limits remain in force. |
| Remote and MCP proxy errors | Typed output failure retained for the caller; no implicit mutation replay. |
| Session-history materialization | Existing MessageSequence shared ownership and pinned SQLite/cursor behavior retained; no whole-history clone is reintroduced. |

The existing zero-copy HTTP write chunker remains a separate protection against
an individual native vectored-write buffer exceeding its platform representation.
These limits do not establish the route or exact input size of a past panic.
