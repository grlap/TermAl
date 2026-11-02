# Engram project settings changes and queued work

Related: [Engram host adapter](engram-host-adapter.md),
[Execution Observability](execution-observability.md) and the
[HTTP routes](../architecture.md#http-api).

A project settings change holds the project's reset fence while its Engram
connection transition applies. The fence belongs to the exact transition
generation that claimed it (`engram_connection_reset.rs`, with
`EngramProjectResetFences` in `state.rs`). It fences connection replacement
and the teardown of the old Engram child, and nothing else: it never decides
whether a turn may run. Observation preferences do not take it.

Prompts arriving while the fence is held stay in the durable queue. When the
owner releases the fence, they continue through ordinary dispatch against the
new settings, keeping explicit pauses, FIFO order and the Human priority
described in the [host adapter](engram-host-adapter.md#status).

Authority, store, enable and disable changes use their revocation and teardown
lifecycle. Records an earlier build left (control requests, grants,
checkpoint debts and receipts) stay with their existing owners, readable as
stored; a settings change neither settles nor replays them
([records from earlier builds](engram-host-adapter.md#records-from-earlier-builds)).
