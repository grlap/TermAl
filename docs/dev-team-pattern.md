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

Roles are ASSIGNED BY THE HUMAN at pairing time — "Sol" and "Fable" are
the original cast; substitute your own sessions. A joining agent learns
its role, its counterpart, and its pair mailbox from the human's
assignment message plus `termal_list_mailboxes` (§7.1). Work items cited
as `tm-*` throughout are beads — the `bd` issue tracker; run `bd prime`
for its workflow.

**Sol — implementer-investigator.** Writes code, runs experiments, freezes
diffs, runs parent gates, captures field data, files and closes beads. Owns
the working tree. Measurement hygiene is part of the role: bracketed
baselines (B-O-O-B: Baseline, Optimized, Optimized, Baseline — the
brackets expose host drift), canonical-response SHA parity across modes,
per-phase telemetry captured with every run, source-grounded pushback when
the referee's hypothesis contradicts pinned code.

**Fable — adversarial referee-architect.** Challenges designs *before*
implementation, audits frozen diffs read-only, rules on governance questions
with pre-written decision criteria, maintains the prediction/retraction
ledger. Writes nothing into the repository (advisory content rides the
durable mailbox — §7.1; the one sanctioned exception is §7.2.6). Verdicts
are grounded in its own greps and test runs, never in the implementer's
summary alone.

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
6. **Empirical gates are the neutral judge.** Examples from the source
   epic — build your project's equivalents: full-table natural-key dump
   parity, counter identity, canonical-response SHAs, anchored corpus
   counts. When two models disagree, the dump settles it. Exactness is
   never argued from reasoning alone when it can be proven from bytes.
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
4. **Durable content lives in the durable mailbox; the notification is a
   pointer.** [AMENDED by §7 — originally this said "shared file store".]
   Full verdicts, checklists, and contracts go INTO the mailbox message
   body (256 KiB cap fits every verdict this pattern has produced), or to
   beads for repo-permanent decisions. Pull-based reading makes queue
   order irrelevant.

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

**Infrastructure wishlist** — BUILT. What was wished: supersedes
metadata, sender-visible turn state, a formalized per-pair mailbox, a
real interrupt flag. It became TermAl's durable neutral mailboxes
(tm-uwx.10), battle-tested the night it shipped; see §7 for what
survived contact with the field and what is still owed.

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
  the pattern survived but should not depend on them. Resolved: the bridge
  was replaced by durable mailboxes (§7). The scratch-directory fallback is
  RETIRED by constitutional ruling — agents are TermAl sessions, so any
  state where TermAl is down has no agents alive to read files. `.collab/`
  is archive only.
- **Authority drift.** Prevented only by keeping the human's authorities
  written and per-changeset. Commit/push authority is whatever the HOST
  repo's instructions grant — in this repo, both are per-instance
  approvals; nothing here overrides that.
- **Defense in depth is the point.** Over the pattern's field history,
  every layer — referee, implementer, formal gate, even the freeze
  mechanism — failed at least once, and every failure was caught by a
  different layer. Keep all the layers.

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

## 7. Field addendum — the durable-mailbox era (2026-07-23/24)

Sections 1–6 describe the pattern as distilled from PhoenixCodeNav. This
addendum records what happened when its §3 wishlist was actually built
(TermAl durable neutral mailboxes, tm-uwx.10) and dogfooded the same
night on a live incident — including the parts that broke. Every rule
below was paid for by a specific failure; none is speculative.

Supersession map for §1–6 (everything not listed survives as written):

| §1–6 rule | Status |
|---|---|
| §1 "advisory files in scratch directory" | AMENDED — advisory content rides the mailbox (§7.1) |
| §3.1–3 sender/receiver etiquette, stamps, condition tables | SURVIVES verbatim on the mailbox channel |
| §3.4 durable content location | AMENDED in place — mailbox bodies, not files |
| §3 "reserved for true urgency" preview one-liner | SURVIVES until the urgent/stop message class (tm-uwx.3) ships, then retired |
| §3 infrastructure wishlist | BUILT (this section) |
| §5 scratch-directory fallback | RETIRED — `.collab/` is archive only |
| §5 commit pre-authorization clause | CORRECTED in place — host-repo authority governs |

### 7.1 What the channel became

- One durable mailbox per participant set is the ordering domain; topic
  is message metadata, never a separate mailbox. Messages are immutable
  rows: dense per-mailbox sequence, sender/target snapshots, class,
  topic, state stamp, full body (bodies live only in the database and
  never enter notifications or prompts).
- Sends carry a caller-supplied idempotency key; a retry returns the
  ORIGINAL receipt marked `duplicate` — the resend failure class is
  closed by construction.
- Each participant holds one forward-only processed cursor, advanced by
  explicit acknowledgement after processing, never by reading. Cursors
  double as read receipts, which eliminated acknowledgement chatter
  entirely.
- Notifications are compact metadata that teach the workflow inline
  (list → read → acknowledge). They are pointers, never state: any
  notification must be a safe no-op against the current cursor, because
  stale and duplicate deliveries are a matter of WHEN, not IF.
- `.collab/` (the file-mailbox predecessor) is archive only. The old
  "degraded-mode fallback" rationale was struck by constitutional
  ruling: agents ARE TermAl sessions, so no agent survives the outage
  the fallback imagined serving. Its historical "doorbell" (a message
  whose only content pointed at a file) is retired with it — bodies now
  travel inline.

**Mechanics — the complete tool vocabulary.** These four MCP tools are
the entire channel; a session with them needs nothing else to
participate:

```
termal_send_to_session   {sessionId (id or name), message,
                          idempotencyKey, topic?, stateStamp?, class?}
                         → durable receipt {mailboxId, messageId,
                           sequence, unreadDepth, duplicate,
                           notificationDisposition}
termal_list_mailboxes    {} → your mailboxes, participants, YOUR
                         processedThrough cursor, unread counts
termal_read_mailbox      {mailboxId, afterSequence?, limit?} → ordered
                         messages; reading never advances the cursor
termal_acknowledge_mailbox {mailboxId, expectedProcessedThrough,
                          processedThrough} → forward-only CAS after
                          processing
```

Sends are session-addressed; the pair mailbox is created lazily on the
first send between two sessions. Put the §3 etiquette INTO the fields:
`stateStamp` carries the domain stamp, `topic` the thread, the body the
condition→action table. `class` is `routine` only until tm-uwx.3.

### 7.2 Rules earned from incidents

1. **Freeze fingerprints** (binds both agents). Every freeze declaration
   carries three digests, computed with EXACTLY these commands so
   declarer and auditor cannot diverge:

   ```
   git status --short | shasum -a 256
   git diff --binary | shasum -a 256
   git ls-files --others --exclude-standard | sort \
     | xargs shasum -a 256 | shasum -a 256
   ```

   Scope is the product tree only — coordination artifacts are excluded
   by gitignore, because any digest covering the channel is invalidated
   by the declaration itself. The auditor recomputes at audit start AND
   end; any mismatch voids the audit, and the implementer re-declares
   with fresh digests plus a note naming what changed. Placeholder
   values are an invalid declaration (bounce, don't audit). A worked
   declaration is one mailbox message: stamp line naming the frozen
   scope, `Type: freeze`, the change summary, and the three full-value
   digests. Paid for by: a frozen tree that drifted mid-audit and was
   caught only by reading the same region twice, and a declaration that
   raced its own template substitution.
2. **Atomic placement** (binds anyone writing files a reader watches).
   Compose outside the watched directory, move in with one rename, then
   signal. Paid for by: two read-races (a half-written audit file; a
   template whose placeholders were read before substitution).
3. **Schema-first for wire params** (binds both agents). Before shipping
   any new protocol field, check the installed peer schema/binary for
   capability gates. Paid for by: `thread/resume.excludeTurns` shipping
   without its `experimentalApi` initialize capability — which bricked
   every Codex session including the implementer's, and cost an extra
   full rebuild/restart cycle.
4. **Never open the host's live database externally** (binds both
   agents) — not even read-only. Diagnostics go through the host's HTTP
   API, process metadata, or on-disk artifacts the host does not hold
   open. Paid for by: an external `sqlite3 ?mode=ro` census that killed
   TermAl mid-investigation and cascaded into metadata loss (§7.2.5's
   trigger).
5. **Loud-fail on asymmetric state loss** (product-engineering
   requirement, not agent behavior). A store that finds its metadata
   missing while its sibling rows survive is looking at data loss,
   never a fresh install, and must say so instead of defaulting. Paid
   for by: a silently emptied thread-suppression set re-importing
   fifteen months-old threads as phantom sessions (tracked: tm-p9p).
6. **Emergency role exception.** The referee may edit product code only
   on explicit constitutional authorization, with the change minimal,
   attributed, gated, and submitted for implementer post-hoc review.
   Precedent: the `experimentalApi` amendment — authorized after the
   half-fix bricked the implementer session itself, reviewed CLEAN
   unchanged the same night. The exception exists so the wall can bend
   in a crisis instead of the system deadlocking; the post-hoc review
   is what keeps it a wall.

### 7.3 Roadmap — open improvements

1. **Single channel** (policy half DONE — §3.4 as amended already
   requires mailbox bodies; engineering half open): retire the
   restart-fragile file monitors and stop producing archive-file bodies
   anywhere the policy has not yet reached.
2. **Retire the wake-turn tax.** A routine notification currently mints
   a FULL agent turn for the receiver; stale or no-op wakes burn real
   turns. Routine unread should surface as a header line in the
   receiver's next genuine turn; dedicated wake turns are reserved for
   the urgent/stop class (tm-uwx.3).
3. **Self-describing protocol** (constitutional ask). A session must be
   able to join with ZERO pre-loaded skills. The inline half already
   works (notifications and tool descriptions teach the next step —
   list → read → acknowledge is followable unaided, and §7.1's
   mechanics box now covers the initiator side). OPEN: a guidance MCP
   function (working name `termal_collaboration_guide`) returning this
   pattern and the mailbox contract on demand, so any session can
   bootstrap itself, mint its own local skills, or store the guidance
   as beads memories.
4. **User visibility** (constitutional ask). The human must have a
   first-class view of agent-to-agent conversations. The read-only
   inline viewer (the mailbox card inside a participating session's
   conversation) exists; still owed: the dedicated workspace mailbox
   tab (tm-d09) and a workspace-level mailbox list, so no exchange is
   discoverable only through one session's conversation.
5. **Fingerprint helper.** One repo-provided command computing exactly
   the three §7.2.1 digests, so declarer and auditor never copy-paste
   the pipeline separately.
6. **Queued-wake hygiene** (diagnosed in the field, bead ids pending
   implementer filing) — three separable items: (a) dispatch-time
   revalidation of queued wakes against the current cursor; (b)
   ack-time sweep of emptied wakes; (c) workspace-watcher ignore-list
   additions so coordination writes (`.collab/`, `.beads/`) stop being
   attributed to whichever session happens to be mid-turn in the shared
   worktree.

### 7.4 Scorecard

One incident-rich evening: fourteen durable messages, zero lost or
duplicated, across four host restarts and one hard outage. The gate
caught four referee misses; the referee caught one hole in a
gate-mandated remedy; each side retracted once, caught by the other
within the hour; the one emergency role-wall crossing was reviewed
CLEAN by the side crossed into. The defense-in-depth conclusion lives
in §5, where it belongs.

### 7.5 Glossary (terms the pattern assumes)

- **beads / `bd` / `tm-*`**: the issue tracker and its ids; `bd prime`
  prints its workflow.
- **freeze declaration**: the mailbox message pinning a working tree
  for read-only audit — scope, change summary, three §7.2.1 digests.
- **doorbell**: retired; a pointer-only message from the file-mailbox
  era.
- **wake turn**: the agent turn a mailbox notification currently mints
  for its receiver (§7.3.2 wants routine ones gone).
- **parent gates**: the implementer-side quality battery (fmt, check,
  focused and full test suites) run before any freeze.
- **fan-in**: the moment a multi-reviewer round's findings are merged
  and ranked.
- **queue wedge**: the historical cross-session-messaging outage that
  forced improvised channels and motivated the mailbox.
- **conversation card / inline viewer**: the read-only mailbox view
  embedded in a participating session's conversation.
- **constitutional**: an authority or ruling held by the human layer
  (§1), never assumable by an agent.
