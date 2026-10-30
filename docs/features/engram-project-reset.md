# Engram tier changes and queued work

Related: [Engram host adapter](engram-host-adapter.md) and the
[HTTP routes](../architecture.md#http-api).

Changing turn-gated control on the same enabled authority uses one 21-second
monotonic control budget, beginning when the project reset fence is acquired.
Quiescence, checkpoint arbitration, transport and durability acknowledgements
consume its remaining time. Another session does not receive a fresh budget.
An exhausted budget before commit refuses the settings replacement. A timeout
does not prove that an original operation failed to execute: original requests,
receipts and recovery holds remain with their existing owners.

Same-connection tier OFF retains its existing durable containment contract.
Unresolved sessions stay excluded after the project-wide fence is released.
Background Stop work uses its existing exact runtime owner and holds a slot in
the existing capped scheduler until that work finishes. An unavailable slot or
an expired reset budget retains that interruption with code
`provider_stop_deferred` in the existing recovery view. It needs no queued
prompt or enabled tier. The original runtime token and active-turn generation
are captured before deferral and rechecked when Stop claims its runtime;
replacement retires only the obsolete scheduling intent, leaving the original
control obligation excluded. Failed handoff retains the duty with the existing
retry backoff. Stop and queue work share eligibility aging and the cap. For a
fixed eligible backlog, the service bound below applies with T covering the
whole interruption and cleanup; a capacity deferral consumes no attempt.
After the initial settings/exclusion acknowledgement, that Stop is eligible
while its own originating project reset fence is held. A foreign reset or an
unexplained session hold defers admission without consuming an attempt or
service age. The scheduler rechecks the captured project and reset generation
when selecting work, admitting its slot and claiming Stop; an already-owned
interruption keeps its cleanup custody if a later fence appears.
Control-process cleanup likewise retains the exact retiring process and its
existing reaping worker;
another process is not admitted until that owner confirms cleanup. Unknown
cleanup is a hold, not an ordinary control-availability fallback.

Replacement SessionBind calls are lazy. A settings change does not construct
or bind targets for idle historical sessions. Old checkpoint targets are built
only for open grants or in-flight owners, and retain their original authority.

A completed `control_session_not_bound` error may enter the existing strict
same-authority absence inspection after its checkpoint invocation releases
the slot. Save validates that exact inspection reservation and preserves the
canonical Unknown checkpoint, original routing token and grant, and admission
exclusion. Absence evidence permits the settings change; it is not a receipt
settling the historical checkpoint and does not authorize a fresh bind.
The inspection uses the exact original checkpoint recovery view. Generic
binding remains excluded by that same duty; it is not used to reconstruct an
admission target. Current settings, actor, child authority shape and every
competing exclusion are revalidated before the reservation is used.

Authority, store, integration-enable and integration-disable changes retain
their existing revocation and teardown lifecycle. This tier-change budget does
not certify those transitions as bounded, or prove capability invalidation
after a failed producer revocation and failed provider detach. Their progress
projection explicitly reports `bounded: false` and `remainingMs: null`.

## Progress and refusal

`GET /api/projects/{id}/engram` returns `reset` and the **committed** `enabled`
and `turnGatedControl` values. With no live reset, `reset` is null. During one,
it reports generation, phase, elapsed time, bounded/deadline status, whether
settings committed, and completed/total work items in the current phase.
Counts describe items visited by that phase, not successful remote settlement.
They default to one operation for phases without a batch. The projection does
not disclose session selectors, credentials, paths or checkpoint payloads.

An overlapping PATCH returns 409 with code `engram_project_reset_in_progress`
and the same redacted reset information, before readiness admission. It does
not queue or supersede a rollback. Expiry reports zero remaining time and
`deadlineExpired: true`, not an expected successful completion. A bounded
pre-commit expiry returns `engram_project_reset_deadline` with outcome
`settings_not_applied`; prior checkpoint effects and exact recovery duties
remain valid. Existing runtime cleanup can refuse a tier change with
`engram_project_reset_runtime_held`. Other bounded safety refusals use
`engram_project_reset_refused` with the current phase and
`outcome: settings_not_applied`.

## Queue release and fair service

Fence release and registration of exact existing queue heads share the same
StateInner cut. Registration is not dispatch or delivery. The existing Engram
retry scheduler serves activations and retries under its unchanged cap of four.
Activations use ordinary dispatch, preserving explicit pauses, original
authority/checkpoint/cleanup holds and the head's existing budget. Cancellation,
replacement or a newer reset cannot transfer an activation to a successor.

Initially eligible work is ordered by due time. Once observed, its runtime
eligibility age survives cap deferral. An admitted attempt consumes that age;
even a failed or no-op attempt rejoins behind already eligible heads. Later
arrivals therefore cannot indefinitely outrank an eligible head.

Service is separate from the reset fence budget. For a fixed eligible backlog
of B heads, four available slots, a complete attempt bound T, and tick interval
I (two seconds in production), a conservative service bound including a current
batch is `(1 + ceil(B / 4)) * (4*T + I)`. The factor four also covers the
existing sequential fallback when scoped worker creation fails. The scheduler
joins its batch before the next tick. T must cover the complete held-slot
lifetime, including delivery, acknowledgement and cleanup; one RPC's timeout
alone does not establish it. Safety-held or explicitly paused heads are not
eligible, and nonterminating competing work invalidates this service premise.

The control budget bounds admitted external work under responsive storage and
OS primitives. Finite local traversal, lock acquisition and synchronous storage
calls are not cancellable wall-clock guarantees under a stalled OS or store.
Expiry never fabricates a commit acknowledgement, checkpoint receipt or reap.

## Owners

`EngramProjectResetFences` owns runtime progress and the generation. The phase
timer shares that owner's monotonic clock. Queue scheduling stays in
`engram_retry_schedule.rs` and ordinary dispatch in `turn_dispatch.rs`.
`engram_control_transport.rs` retains child cleanup; checkpoint and containment
owners retain durable original-operation custody. No new persisted queue or
reader floor is introduced.
