# Batch Full Gates

**Current limit.** TermAl no longer binds one full gate to the other items of
a batch. It sends no `verification_bind`, so a gate earns credit only for the
item whose registered verification records it, and each other item needs its
own registered run ([test evidence](engram-host-adapter.md#test-evidence),
[the test guide](../test.md#credit-for-a-background-or-detached-gate)). Do not
plan a landing on batch credit.

See also: [batch full gates in the host adapter](engram-host-adapter.md#batch-full-gates).

## What remains

- **Members at a launch.** A carried launcher full gate whose root other items
  of the session also name still records those members, with the fence each
  claim had then (`engram_launch_batch_members`). That record is history only:
  nothing is bound or sent from it.
- **Stored owed binds.** Owed binds an earlier build stored stay in the
  session's history as raw records (`engramOwedBinds`). They are read as
  stored, never sent, retried or settled.
- **Bound records Engram already holds.** A record Engram bound to a member
  before this change stays on that member's run. The acceptance binding read
  still reads it like a native record for the member's criterion
  (`AcceptanceBindingEvidenceBound`): the evaluation brief names it as the
  criterion's closure and says which item's check it was bound from, and
  TermAl checks that the record's producer precedes the original record on
  the original item's run, and that the original ran at the record's
  revision.

## History

Earlier builds credited a carried gate to its own item at a checkpoint, then
turned each other member into an owed bind and sent `verification_bind` from a
thread of its own, one idempotency key per gate and member
(`termal-verification-bind:{original}:{work_id}`). That sender, the owed-bind
executor and the checkpoint it waited for have been removed with turn gating.
The request shape and its receipt handling remain in
`src/engram_verification_bind.rs`, but alone they send nothing and grant no
credit.

## Related

- [Engram host adapter](engram-host-adapter.md#batch-full-gates)
- [Test guide](../test.md#credit-for-a-background-or-detached-gate)
- [Host architecture](host-architecture.md)
