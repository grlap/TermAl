# Session recovery after a host restart

A host restart can interrupt a root session before its agent finishes the
request. Once inventory is available and the listener is ready, TermAl queues
one ordinary recovery opening for an eligible local root that was stored as
`Active`, `Approval` or `Stopping`. It includes the existing restart notice,
the restart time and the number of queued prompts saved at that interruption.

The opening asks the agent to recover its original request under its session
instructions. It grants no tool, approval or execution authority. An approval
that expired in the restart stays interrupted. No interrupted tool call is
replayed by the recovery mechanism.

## Eligibility and queue order

An idle root without an interrupted turn gets no new opening. Delegation
children, including children known only through a saved delegation record,
are excluded. Existing holds suppress the opening: a paused automatic queue,
Engram operator pause, a stopped-prompt marker or a pending observation delete.
There is no additional setting or opt-out flag.

The normal user Stop pauses automatic dispatch and therefore suppresses this
recovery. A programmatic Stop that does not pause the queue or leave an Engram
Stop marker can still recover if the host restarts while it is `Stopping`.

The recovery uses the existing prompt queue and content-specific persistence
fence. Before the first handoff, the persist writer must acknowledge its exact
queued content. The opening sits after protected queue heads and the leading
run of runnable restored mailbox wakes, and before ordinary saved work.
Mailbox corrections therefore reach the agent before it resumes the old
request. After recovery finishes, normal queue draining continues in order.

An ordinary user prompt already ahead of a different workflow activation still
acts as its usual queue barrier. A root without an interrupted turn and without
a committed workflow activation keeps its queue dormant until a user action
or new wake activates it. See [Agent mailboxes](agent-mailboxes.md) for unread
mail recovery, delivery status and acknowledgement behavior.

## Repeated restarts and rejected sends

A persisted marker ties the opening to its interruption. While the opening is
queued or active, another restart keeps that marker and does not mint a second
opening. This also applies when a restored mailbox turn is active ahead of
the still-queued recovery. A later ordinary turn clears the completed marker,
so a distinct later interruption can receive its own opening.

A synchronous runtime rejection from the durability-ACK, boot-orphan,
turn-lifecycle queue drain or explicit queue Resume that proves the opening
was never sent restores it through the existing workflow requeue owner under a new prompt id;
the marker follows that id. The failure remains visible, and recovery waits
for a later activation or boot instead of repeatedly retrying the same dead
runtime. Accepted, ambiguous or asynchronously scheduled handoffs are not
requeued by this rule. A saved
undelivered opening is dispatched through the existing orphaned-workflow
dispatcher at the next boot.

Queue pauses, explicit cancellation and the existing delivery holds retain
their normal meaning. This mechanism neither resumes a stopped delegation
child nor manufactures an Engram turn grant. See
[Engram host integration](engram-host-adapter.md#mailbox-and-stop-behavior)
for the ordinary handoff and Stop boundaries.
