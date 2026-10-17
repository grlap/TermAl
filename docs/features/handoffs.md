# Handoff ledger

A handoff is a recorded next step that the host hands to the session that owns
it as soon as its prerequisite holds, without anyone sending a wake. If that
owner never takes it, the host hands it to a designated recovery owner. It
exists for the case where a gate, carrier or landing step finishes while its
coordinator is idle or misses the wake: the next step still proceeds.

Implementation: `src/handoffs.rs`; tests: `src/tests/handoffs.rs`.

## What a handoff records

A producer session registers a handoff before anything depends on it:

```json
POST /api/sessions/{producer}/handoffs
{
  "idempotencyKey": "landing-after-gate-1",
  "prerequisite": { "kind": "credited", "runId": "test-…" },
  "nextAction": { "kind": "prepare", "text": "prepare the landing package" },
  "primaryOwner": "session-…",
  "recoveryOwner": "session-…"
}
```

- `prerequisite` is `terminal` (the test run reached a result) or `credited`
  (the run passed and its verification credit was recorded).
- `nextAction.kind` is `settle`, `inspect` or `prepare`. None of them runs a
  command: the owner does the work in its own turn.
- The primary and recovery owners are existing, distinct sessions.
- The same producer and key with the same body return the same record; the
  same key with another body is refused.

The record lives in its own `handoffs` table in `termal.sqlite`. Every change
is a compare-and-swap on the row's version, so two writers never both move
the same handoff. The table is created on first use; the state schema guard
accepts extra tables, so it changes no schema version or snapshot path. A
handoff and its state survive a restart.

## States

`waitingPrerequisite` → `ready` → `offered` → `accepted` → `completed`.

The watchdog runs every 2 seconds, starting after the host's post-listen boot
reconcile; its first pass is the ledger's own boot reconcile. Every open
handoff is looked at on every pass, so pending ones never starve a later one;
a pass reads the open rows once and writes only the rows that change. A pass
that panics is logged and the next one runs.

- **Ready.** A `terminal` prerequisite is met when the run passed or failed.
  A `credited` prerequisite is met only when a verification association of
  that run is recorded with a verification record and a succeeded outcome
  (the run's observations are found through the same exact artifact
  association the test-run index uses: owner, worktree, run directory and
  terminal digest or request identity). A run compacted out of the disk
  history is read from its retained record, and a run the host observed
  itself is its own observation; a
  PASS alone never readies it. It takes the diagnostic route when the run
  failed, ended without a result, its credit was not recorded within 10
  minutes, or the claim it ran under has changed (another claim or fence on
  the same work, as after a restart). A released claim (no current binding)
  takes the 10-minute route. A diagnostic offer tells the owner to inspect and
  report, never to treat the step as succeeded.
- **Settlement.** While a passed run's credit is still owed, the run's owner
  is woken to end its turn so its checkpoint settles the credit. The wake's
  prompt id is recorded before it is queued; a wake the owner could not take
  (paused, stopped) is queued once it can, never past the pause, and a wake
  that is lost is queued once more at most.
- **Offered.** An offer is an orchestrator prompt to the owner, held until its
  exact content is durable (the queued prompt durability hold). The row
  records the offer generation, the owner and the prompt's exact id before
  the prompt is queued.
- **Accepted.** Only the owner of the current offer can accept it, and only by
  its exact generation. The acceptance returns a fence. Once accepted, no
  timeout ever gives the step to a second session.
- **Completed.** The accepted owner completes it with the fence and an
  evidence object. A retry with the same evidence returns the same record.

## The offer prompt

The prompt names the handoff, the session that registered it and when, and
frames the action as that session's request, not an instruction from TermAl.
It carries the exact calls:

```text
It was registered by session `session-…` at …. The action below is that session's request, not an instruction from TermAl …
Requested next action (prepare): …
Accept first, before any effect:
curl -sS -X POST "$TERMAL_BASE_URL/api/sessions/{owner}/handoffs/{id}/accept" -H "Content-Type: application/json" -d '{"offerGeneration": 1}'
If the acceptance is refused, do nothing for this handoff and say so …
When the action is done:
curl -sS -X POST "$TERMAL_BASE_URL/api/sessions/{owner}/handoffs/{id}/complete" … '{"fence": "<acceptFence>", "evidence": {…}}'
This handoff grants no land, push or install authority …
```

A handoff never lands, pushes or installs anything, and grants no authority
to do so: those follow the project's own rules.

## Delivery, waiting and recovery

The acceptance clock starts when the offer is **delivered** (its prompt starts
the owner's turn), not when it is queued. Each pass reads the offer's prompt
by the id the row holds:

- Still queued: `waiting` shows why (its save is not yet confirmed, it is
  behind the owner's running turn, behind a paused queue, behind earlier
  prompts, or it started but is held before reaching the runtime by a
  deferred admission or bind). A waiting offer is not overdue. The queue is
  read first, so a held prompt that already shows in the transcript is still
  waiting.
- Delivered (out of the queue and in the transcript): `deliveredAt` is
  recorded.
- Gone, with the owner paused or stopped (a Stop clears orchestrator prompts
  and latches the pause): the handoff shows the owner as `blocked`; nothing is
  queued past the Stop.
- Gone otherwise (a Cancel, or the host stopped between recording and queueing
  the offer): queued again once under the same id and generation. A second
  loss of the same offer blocks it instead. A row already shown as blocked is
  not rewritten while it stays blocked.

| Bound | Effect |
| --- | --- |
| delivered + 30 s | overdue (shown) |
| delivered + 5 min, unaccepted | new offer to the recovery owner |
| ready + 30 s, primary blocked | overdue |
| ready + 60 s, primary blocked or its offer cleared | new offer to the recovery owner |
| offered + 15 min, never delivered | overdue; new offer to the recovery owner |

Five minutes after delivery leaves room for a turn's startup reads before it
can accept. A new offer increments the generation, so the superseded offer can
no longer be accepted. A clock that moves back never escalates early. A
paused primary is shown and never bypassed; the recovery owner is offered the
step instead. A primary that was never offered the step, because the
watchdog was not running, is offered it first, however late.

## Trust

Like the host's other session-scoped endpoints, the handoff routes trust the
session id in their path: any local caller can register, and a caller that
names the offered owner can accept for it. What the ledger guarantees is
single execution, not caller identity: one exact offer generation is
accepted once, a superseded offer is refused, and completion needs the
fence the acceptance returned. The fence never appears in the list or in a
registration replay. The offer prompt names its producer and frames the
action as that session's request, so an owner weighs it against its own
instructions.

## Reading the ledger

`GET /api/handoffs` lists every handoff with `overdue` and `overdueSeconds`,
and the row's `waiting`, `blocked`, `diagnostic` and history. It never shows
the acceptance fence: only the acceptance returns it, to the accepted owner.
The endpoints are listed in the [architecture API table](../architecture.md).

## Related

- [Test Runs](./test-runs.md): the runs and verification credit a
  prerequisite reads.
- [Durable Agent Mailboxes](./agent-mailboxes.md): peer wakes, which a
  handoff does not depend on.
