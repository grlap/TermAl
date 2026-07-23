# The Sol + Fable Dev-Team Pattern

Adversarial pair collaboration between two AI agent sessions, with an
independent review gate and a human constitutional layer. Distilled from the
PhoenixCodeNav `epuc` performance epic (2026-07-20 → 2026-07-23): five shipped
optimization rounds, three instrumentation rounds, eight adversarial diff
audits, two falsified-and-converted experiments, zero Critical/High findings
surviving any gate, and cold exact references taken from 32.7 s to
milliseconds-warm with exactness byte-parity-gated at every step.

This document describes the pattern so it can be reused deliberately rather
than re-derived.

## 1. Roles

**Sol — implementer-investigator.** Writes code, runs experiments, freezes
diffs, runs parent gates, captures field data, files and closes beads. Owns
the working tree. Measurement hygiene is part of the role: bracketed
baselines (B-O-O-B), canonical-response SHA parity across modes, per-phase
telemetry captured with every run, source-grounded pushback when the
referee's hypothesis contradicts pinned code.

**Fable — adversarial referee-architect.** Challenges designs *before*
implementation, audits frozen diffs read-only, rules on governance questions
with pre-written decision criteria, maintains the prediction/retraction
ledger. Writes nothing into the repository (advisory files live in a shared
scratch directory). Verdicts are grounded in its own greps and test runs,
never in the implementer's summary alone.

**The formal review gate — separate from both.** The repository-required
dual `/review-changes` round (independent Codex + Claude `/review-code`
children) is the only authority for check-in. The referee's audits explicitly
never substitute for it. This is the rule that keeps advisory involvement
from becoming false confidence.

**Greg — the constitutional layer.** Holds all authorities the agents may
not assume: push approval (per changeset, no standing grant), review-gate
exceptions, hardware/environment decisions, remote field captures, and final
priority calls. The pattern works because these authorities are explicit,
written, and never inferred.

## 2. The operating protocol

1. **Design review before implementation.** The implementer proposes a
   concrete plan; the referee challenges exactness, scope, and ranking, and
   the disagreement is resolved in writing before code exists. Deliberate
   disagreement is welcome and has gone both ways (e.g. the referee's
   conservative-retention proposal was correctly overturned by the
   implementer's counter-parity argument; the implementer's cache-first plan
   was correctly overturned by the referee's missing-invalidation-primitive
   finding).
2. **Frozen-diff audits.** Before the formal gate, the implementer freezes an
   exact path inventory and stops editing. The referee audits that frozen
   state read-only, with findings ranked Critical/High/Medium/Low. Without
   the freeze, read-only auditing is theater. No edits land under an active
   review round; late findings are reconciled as beads or, if the gate has
   not spawned, via an explicit unfreeze-and-regate decision with its cost
   stated.
3. **Predictions before runs.** Every experiment writes its expected outcome
   (with numbers) into the bead before executing. Confirmed predictions bank
   levers. Falsified predictions are treated as the more valuable outcome:
   "if the result lands outside the window, the model is wrong somewhere
   interesting" — this clause produced the two deepest mechanism discoveries
   of the epic (the named-parameter binding quadratic; the blocked-thread
   second buffer).
4. **Retractions in writing.** When either side is proven wrong, the
   correction is stated plainly, attributed to oneself, and recorded — no
   softening. Examples on the referee side: a premature single-arm
   confirmation against its own n>=2 protocol; a hardware misread from an
   in-flight edit; an overstated "banked win." Examples on the implementer
   side: phase misattribution, contaminated baselines invalidated without
   being asked. An adversarial pair that cannot retract cleanly produces
   stubborn noise instead of convergence.
5. **Verify claims in both directions.** The referee greps and runs tests
   before endorsing ("verified absent," "verified exactly"); the implementer
   reads pinned dependency source before accepting the referee's claims about
   library internals. Trust the person; verify the claim.
6. **Empirical gates are the neutral judge.** Full-table natural-key dump
   parity, counter identity, canonical-response SHAs, anchored corpus counts.
   When two models disagree, the dump settles it. Exactness is never argued
   from reasoning alone when it can be proven from bytes.
7. **Mechanism over statistics on noisy hosts.** Wall-clock A/Bs adjudicate
   only large effects. Small effects are adjudicated by instrumented
   mechanism evidence (who waits on whom, occupancy regimes, per-phase CPU),
   which is immune to host drift. Decision thresholds are written before the
   data arrives so outcomes route themselves.

## 3. Communication — batch semantics, not realtime

TermAl cross-session messaging is TURN-BASED AND BATCHED by design: messages
queue behind the receiver's active turn, and the receiver processes an
accumulated batch at its next turn boundary, then replies. Turn lengths are
asymmetric — the referee's turns are short (advisory), the implementer's are
long (implementation + gates) — so the fast side's messages accumulate. This
is email etiquette, not chat: the model rewards complete thoughts and
punishes bursts. Treating queuing as a failure was the original mistake; the
etiquette below is what actually works.

**Sender etiquette (binds the fast side hardest):**
1. **One logical message = one message.** No bursts, no preview-line
   duplicates of the same content, no resends on suspicion. If content must
   change before it was read, send a SUPERSEDES message that names what it
   replaces — never a near-duplicate.
2. **State-stamp every substantive message**: "composed against <inventory /
   HEAD / gate-state>". Crossings then self-diagnose: a recipient whose
   world has moved past the stamp knows to reconcile rather than obey.
3. **Write rulings as condition→action tables, never imperatives.** "If the
   gate has not spawned → X; if spawned → Y; if reviewers raise Z at
   Medium+ → their finding governs." Timing-proof messages cannot rot in the
   queue. This is the single highest-value async adaptation: send the
   DECISION FUNCTION ahead of the data instead of waiting to rule on the
   result.
4. **Durable content lives in the shared file store; the message is the
   notification.** Full verdicts, checklists, and contracts go to the shared
   scratch directory (or beads, for repo-permanent decisions); the TermAl
   message carries the pointer plus the condition table. Pull-based reading
   makes queue order irrelevant.

**Receiver etiquette:**
5. **Announce long turns**: "starting the full gate now (~40 min); batching
   your messages until fan-in." The fast side then coalesces instead of
   resending.
6. **Process the batch as one conversation**: read everything, discard
   superseded items by their headers, reconcile stamps against current
   state, reply once.

**Reserved for true urgency**: single-line messages whose entire content
fits the session preview (visible even mid-turn). Spent only on
round-invalidating events — an inventory contamination, a wrong-diff freeze —
never on impatience.

**Infrastructure wishlist** (worth building if the pattern scales): topic +
supersedes metadata that auto-collapses batches; sender-visible turn state
("receiver mid-turn, N queued"); a formalized per-pair mailbox directory
with an index; a real interrupt flag replacing the preview hack.

## 4. Tiering

The full pattern (design review + per-experiment refereeing + frozen-diff
audit + dual gate) is priced for performance- and correctness-critical work.
For routine changes, tier down to: frozen-diff audit at the freeze point plus
the normal dual gate. The formal gate never tiers down.

## 5. Risks and mitigations

- **Correlated blind spots.** The referee's design advice shapes what the
  Claude gate-reviewer later reviews — partially reviewing its own family's
  thinking. Mitigations that held: the gate stays blind to design
  provenance; both vendors always sit in the formal gate regardless of who
  designed or implemented; the empirical parity gates carry objectivity no
  model shares.
- **Advisory false confidence.** Mitigated by the standing rule that the
  referee's CLEAN verdict is advisory and the dual gate alone authorizes
  check-in.
- **Channel fragility.** The TermAl queue wedge forced improvised channels;
  the pattern survived but should not depend on them. Fix the bridge; keep
  the scratch-directory fallback sanctioned.
- **Authority drift.** Prevented only by keeping the human's authorities
  written and per-changeset. The commit pre-authorization exists in the repo
  instructions; push never has a standing grant.

## 6. The distilled checklist

If only three practices survive adoption elsewhere, keep these:

1. **Frozen diffs** — audits and gates run on exact, stated, unchanging
   inventories.
2. **Predictions first** — every experiment states its expected numbers
   before running, and falsification is treated as a payoff.
3. **Retractions in writing** — both sides amend the record plainly when
   proven wrong, in the same channel where the error was made.

The rest — role asymmetry, dual-direction verification, mechanism-first
measurement, empirical parity as judge — follows from taking those three
seriously.
