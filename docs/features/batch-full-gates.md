# Batch Full Gates

One carried full gate can be recorded for every item of a commit that lands
several items, instead of one 26-minute gate per item. The gate is credited to
the item it was launched for, as any carried gate is, and TermAl then asks
Engram to bind that passed record to each other item of the batch, on each
item's own run (`verification_bind`). Each item's acceptance evaluation can
then cite a passed test on its own run, at the same source revision.

See also: [carried background gates](engram-host-adapter.md#carried-background-gates)
for how a gate is carried and credited, [the test guide](../test.md#credit-for-a-background-or-detached-gate)
for launching one, and [the host architecture](host-architecture.md) (Rule 10,
section 5.4) for why a bound record is not credit carried by another grant.

## Declaring a batch

A batch is declared by holding it. The session that runs the gate:

1. holds each member's claim;
2. names each member's source root to the same worktree as the gate's own
   item (the same root path and the same Git common directory, in the same
   Engram store);
3. gives each member a granted turn on its own claim, in that worktree, on the
   frozen tree, before the gate. That turn reports the root's source basis, so
   Engram records the member's newest sighting at the gate's revision. A focus
   switch alone records nothing, and TermAl never makes a sighting up;
4. launches the full gate in the background or detached, in that worktree,
   from a turn bound to one of the items.

Only a carried gate records members. A gate run in the foreground is credited
to its own item only.

## What TermAl does

- **At the launch.** For a carried launcher full gate whose root other items of
  this session name, TermAl reads the session's held claims, off the state
  lock and within six seconds, and records each member with the fence its
  claim has then. A member whose claim is missing from a list Engram cut short
  (Engram lists the newest held claims only) is not recorded, and neither is
  any member when the read fails: each is told why.
- **At the gate's credit.** A passed gate leaves its members pending until the
  checkpoint that reports it answers. That answer names the gate's own passed
  record (one record id per verification input, in input order, which Engram
  keeps as a contract). Each member then becomes an owed bind, stored before
  any call. A failed or refused gate binds nothing; a checkpoint Engram refuses,
  or one not answered within 30 minutes, tells each member.
- **Binding.** A thread of its own works the owed binds every two seconds,
  with at most 20 seconds of Engram calls per tick, never on a turn's path:
  1. it reads the member's held claim and its newest sighting, and decides.
     A member is bound only while its claim is held at the fence it had at the
     launch, its root still names the gate's worktree at the same generation,
     and its newest sighting is at the gate's revision; otherwise the member
     is told why, with the remedy when the sighting is not there;
  2. it stores that decision and reads it back from the store, then sends
     `verification_bind` on a later tick. One idempotency key per gate and
     member makes every resend safe: Engram replays a bind it committed and
     never binds twice. Each send holds the session's binding while it runs,
     like a bind: a change of the project's Engram settings waits for it, and
     one that came first stops it;
  3. Engram's answer settles the member: bound, already bound, or refused.
     An uncertain answer (a lost call, a busy store, an unreadable receipt) is
     sent again with backoff (5 seconds to 8 minutes); a refusal of the
     session's credentials asks for a rebind and waits for the fresh token.
     TermAl rebinds for it only while the session is idle, since a rebind
     closes any turn still open; a busy session is rebound by its own next
     turn.
- **Limits.** A member is given up after eight attempts or 30 minutes. One
  never sent is told it is not recorded; one sent may be bound, so it is told
  the result is unconfirmed.

## The lines

Every member line is a line about checks' credit, so a batch's lines merge into
one and never push a source-root line out:

- `the full gate is also recorded for <item> (bound), on its own run.`
- `the full gate is not recorded for <item>: <why>.` The gate's record is not
  bound to that item; run its gate again, or give it the missing turn first.
- `the full gate is unconfirmed for <item>: <why>; it may or may not be
  bound.` Check the item's own run before running its gate again.

## Restarts and resets

From the launch until it settles, every member is in its session's stored
record: in the carried gate's launch marker, a pending batch, or an owed bind.
After a restart, an owed bind is sent again, given a fresh 30 minutes and at
least one attempt; a member of a carried or pending batch is told its gate's
record could not be bound, since that record was never seen. A change of the
project's Engram settings drops every member with a line. While boot recovery
or a bind restores the session's credentials, owed binds wait.

## Acceptance evaluation

A bound record is read like a native one for the member's criterion: the
evaluation brief names it as the criterion's closure and says which item's
check it was bound from. TermAl checks that the record's producer precedes the
original record on the original item's run, and that the original ran at the
record's revision.

## Related

- [Engram host adapter](engram-host-adapter.md#batch-full-gates)
- [Test guide](../test.md#credit-for-a-background-or-detached-gate)
- [Host architecture](host-architecture.md)
