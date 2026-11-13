# Execution Observability

Execution Observability is a per-project setting of TermAl's Engram
integration. With it on, TermAl reports to Engram what each turn of a local
agent session did, after the turn ran. It never decides whether a turn may
run: it replaces turn gating, which has been removed.

The full host contract is in the [Engram host adapter](engram-host-adapter.md).
This brief states what the setting does for a session.

## Turning it on

A local repository takes part in the Engram integration when its root holds a
non-empty `.engram-project` file and the project's Engram integration is
enabled (`enabled`). Execution Observability is a separate project-level
ON/OFF setting, `executionObservability`, on by default: the
**Execution Observability** checkbox in the project's Engram settings, and the
project's status reads `Enabled · observations on` or
`Enabled · observations off`. It takes effect only while the integration is
enabled: automatic observation runs when both are on.
A project setting saved by an earlier build cannot switch it, or anything
else, back to turn gating. The global `TERMAL_ENGRAM_DISABLED` kill switch
turns the whole integration off for every project. Remote proxy sessions never
get it locally ([settings and verification](engram-host-adapter.md#settings-and-verification),
[configuration authority](engram-host-adapter.md#configuration-authority)).

The setting is read when a turn's trace would start. Turning Execution
Observability OFF starts no new traces, so it stops only the automatic reports
of turns, commands, source and checks; a turn already traced still finishes
and delivers its report. It does not change how turns are delivered,
queued, stopped or reset, and it does not turn off the work session: claims,
naming a source root, reads and explicit registered verification keep working.

## What a session gets

With the integration enabled:

- **Work context.** TermAl injects the Engram MCP server into each local agent
  session, so the agent can read, claim, note and complete its work items. It
  adds `engram work next --peek` context at session start and after compaction
  ([start and post-compaction context](engram-host-adapter.md#start-and-post-compaction-context)).
- **Source roots, test evidence and acceptance.** An agent names the worktree
  its claim is measured in ([source root](engram-host-adapter.md#source-root)).
  A test-launcher run selecting committed input automatically or with explicit
  `--input commit` in that worktree is
  sent to Engram as registered verification, the only source of test credit
  ([test evidence](engram-host-adapter.md#test-evidence)), and
  acceptance is judged by an independent evaluator
  ([acceptance evaluation](engram-host-adapter.md#acceptance-evaluation)).
  Acceptance evaluation is temporarily switched off as a blocker until
  Execution Observability is implemented: completing an item does not wait
  for it, but it stays part of the product and can still be requested.

With Execution Observability on as well:

- **Host observations.** When a traced turn ends, TermAl reports it to Engram
  once: how it ended (succeeded, failed, cancelled or unknown), whether it
  changed source, and the checks it ran, each with its kind and observed
  result. Codex, Claude, Cursor, Gemini, OpenCode and Kimi turns report in the
  same shape. A turn a runtime starts by itself is reported the same way.
- **Source sightings.** After a traced turn closes, when the turn's
  attribution is certain and its source can be measured, TermAl also reports
  a measured sighting of the claim's source at its exact revision. A sighting
  tells Engram how fresh the source is; it earns no test credit and records no change.

If the ordinary `content-v1` capture refuses a repository because it contains
Git submodules, the turn still reports its outcome and observed checks.
Source attribution is unavailable: `source_change: null` reports no source
measurement in this case. Null alone does not distinguish this from a
measured-equal close, which also reports null but has a companion source
sighting. A null value alone never proves source was unchanged. For the
gitlink refusal no source sighting or baseline is created, freshness does
not advance, and the checks remain uncredited.
The host keeps a diagnostic explaining the missing source measurement.
This reporting path does not provide a committed-input test-credit workflow
for consumer repositories; that requires separate registered verification.
Other source refusals and temporary capture failures keep their existing
reporting and retry behavior.

## Declared native source verification

Native declared test credit uses endpoint evidence. Before sending a fresh
turn's prompt to Codex or Claude, the host completes an opening snapshot
`S0`: the full `source_id`, the committed declaration and its hash,
and the declared artifacts' pre-images. A matching declared command must be
the first tool activity of that turn. An earlier read, edit, MCP call or
other tool call makes it ineligible. The agent still runs its ordinary exact
declared command, with no manual launcher or wrapper. Other runtime forms
are unsupported by this credit route.

When S0 is unavailable, the notice includes bounded stage names and elapsed
milliseconds, including an operation still running when the caller stopped
waiting. These timings also appear in the host diagnostic; a successful
opening retains its stage summary in the native run request. They contain
no paths, Git arguments or file contents, and do not extend the opening budget.
Source capture allows 30 s; opening reserves another 5 s for its existing
declaration, preimage and authority stages. The source adapter still caps its
own work at 30 s. Timings identify where time was spent, not why it was slow.

The host associates only the original execution newly originating in that
turn; it never adopts an earlier or background execution. `R` is the real,
persisted first **host-observed start**, recorded as `registered_at` and
`check.started_at`. It is not an OS spawn timestamp. Any original provider
timestamp is retained separately. The exact named-root binding must already
exist before `R`; the current session, claim/fence, revision, store,
workspace and root generation remain subject to the registered producer's
existing checks. A delayed acknowledgement never changes `R`.

At the original END callback, before returning to provider-event processing,
the host freezes the actual TRX or Dart JSON artifact judgment. Asynchronous
registration and retries consume that immutable judgment. A later artifact
cannot certify the original execution. Claude's successful tool result is
preserved as reported success, with no invented numeric OS exit; reported
success still requires an artifact PASS. Missing, malformed, failing or
unreadable artifacts cannot inherit a successful command result. Truncated
provider output currently withholds native credit even with an exact exit.

The post-run full `source_id` must equal S0, with the declaration unchanged.
Native verification includes local changes and initialized submodules in
these independent source endpoints. Ordinary turn closing observations
retain their configured source scheme, including content observations of
source left dirty; they are not relabelled as native commit evidence. The
existing local registered producer sends verification. Generic launcher
focused registrations cannot opt into declared native credit by supplying a
declaration, stage label or artifact. Missing or uncertain execution, opening
snapshot, authority or artifact evidence leaves the real command result
visible and earns no credit.

### Canonical source_id and EO credit

The shared source contract is quoted below. See also the
[source-identity reference](source-identity.md#canonical-native-source_id-and-credit).

> source_id = ONE object describing the source state, produced only by `engram source` (Engram owns it):
> - root: commit (HEAD) and tree;
> - submodules: path and actual HEAD for each, from `git submodule status --recursive`; expected pins come from the root commit;
> - changes[]: only the local delta, from `git status --porcelain=v1 -z --untracked-files=all --ignore-submodules=none`, with path, status, origin (for renames), mode or type, and the Git blob id from `git hash-object --stdin-paths` (default filters, no -w), computed over the dirty paths only. Deletions are tombstones. Untracked non-ignored files are included. Ignored files are excluded absolutely. Only dirty or off-pin submodules recurse; an uninitialized or conflicted submodule makes the result unavailable. Clean means changes = [].
> Bounds (Greg accepted: „mysle, ze to rozsadne ograniczenie w wiekszosci przypadkow, git diff jest pusty”): at most 1 MiB of JSON and 8 s; overflow, timeout, drift seen by a stat-and-status recheck, filters, or any error means unavailable. A partial result is never returned.
> Never: watcher events, hashing anything in the .git directory, or a full repository or index listing.
> EO credit (Greg: „to jest wystarczajacy warunek ... mysle, ze i tak jest za silny, ale zacznijmy od tego”): the full source_id at S0 (before the prompt is dispatched) equals the full source_id after the declared test; local changes are allowed. The declared test must be the turn's first tool action; the result goes out as the existing verification_record_v1, with the existing artifact, identity and claim checks. There is no clean-repository requirement and no watcher. Clean is only a landing policy.
> Accepted, documented limits: a change fully reverted between S0 and the after capture; anything git status does not report (hidden index flags, edits inside .git); changes to ignored files.
> Rollout: `engram source` gains the new fields in its existing JSON (no v2); TermAl's and Engram's strict readers are updated in the same window. Consumers then move off content-v1 one at a time. Old records stay readable as stored.
> Rule: no new conditions, mechanisms or gates without Greg's explicit word.

The quoted 8 s bound is historical. On 2026-11-07 Greg changed the source
capture bound to 30 s (Asistant #734): „8 sekund jest za mało. Podnieśc do
30 sec. Takie sztywne limity bardzo duzo nas kosztuja poprawek i marnowanego
czasu.” The 1 MiB output bound is unchanged. This applies to S0, after-run
and received-notice source capture, including the owner's `--timeout-ms`.

A credited declared test runs as the first tool action of a fresh turn.

The opening capture measures the already-authorized named worktree once.
That same validated owner observation supplies its executable pin, source schema
and full `source_id`. The host rechecks its project, configuration, session and
named-root fence before publication. A metadata GET for the project root cannot
supply the named worktree's opening evidence; a later capture uses the retained
pin and a fresh measurement.

This native start-time meaning differs from a launcher's pre-spawn
registration. Native records use the `source_id` input and post-run branch of
`verification_record_v1`; launcher Commit/StagedTree records, chronology guards
and ordinary content identity retain their existing form. The source transport
must be installed before this native producer is enabled. An old server's
unknown-kind refusal is retained; it never triggers a commit-only fallback.

## One rule for a registered committed-input check

Engram owns this rule (Engram's acceptance evaluation docs, "Registered
Git-input evidence"). A registered
committed-input check is a launcher run in the claim's named root that selected
committed input, automatically or with `--input commit`
([committed-input runs](../test.md#ordinary-committed-input-runs---input-commit)).
A criterion resting on it counts only while that root is clean at exactly the
checked commit. Engram reads this from Git itself, not from TermAl's reports.

- **When Git is read.** A new passing evaluation of a claim with such a run
  reads it, including a judgment that cites only notes, and so does the
  `done` that consumes the evaluation. With evaluations off, Git is still read
  for a test-bound criterion, and for a `done` that cites registered evidence,
  directly or through a satisfied acceptance result. A `done` with no
  evaluation, no test-bound criterion and no such citation reads nothing.
- **What refuses.** Among other things: a dirty root, a different commit (even
  with an identical tree), a missing or unreachable root, and a newer failed
  run of the required check. Engram's bounded Git reader also refuses a root it
  can't judge, such as one with configured filter drivers, an uninitialized,
  off-pin or conflicted submodule, or a SHA-256 repository, and reports a read
  past its time or output bound as unavailable, never clean. Uncommitted work
  earns no committed credit: commit it and run again on that commit. Complete
  the item before removing its worktree.
- **What carries over.** Recovering a lapsed claim on the same run and root
  generation keeps the check. A new root generation needs a check there.
- **Limits.** Git is read at the decision, not continuously, so an edit
  reverted to the checked commit before then passes. Ignored files, edits
  hidden by assume-unchanged or skip-worktree flags, what Git's file-mode,
  line-ending and stat-cache rules hide, and edits outside the named root are
  not seen.
- **Turn reports are history.** Their source changes, baselines and arrival
  order don't decide whether the check still counts, and a report that names
  no source location doesn't refuse it. A located change in another root keeps
  its own guard.

Native declared credit is outside this rule. It compares the full
`source_id` instead, at its two endpoints and again when Engram evaluates and
completes, and a different state refuses
([above](#canonical-source_id-and-eo-credit)). Equal local changes at the two
endpoints still record the verification, but TermAl's acceptance admission
requires the owner's clean committed source: a native record with local
changes is refused there, so commit the changes and run the declared test again
([native verification](declared-test-commands.md#native-verification)).
Staged-tree inputs and earlier records keep the rules they were recorded under.

## Delivery and context preparation

When a recognised test starts, an optional placement notice can explain that
its directory belongs to another held claim. The held-claims read runs in
an owned background observer, so runtime frames continue while Engram is
unavailable. A result is published only while the original turn, runtime,
work association, connection and named roots are still current, and no newer
command feedback has superseded it. An absent, failed or stale read does not
change the command's observed check or credit.
The placement read and any received-verification checks share one
thirty-second notice budget. Its held-claim read uses the existing work-binding
command timeout or the remaining budget, whichever is shorter. Host shutdown
may wait for that read and its child cleanup to finish.

An explicitly correlated launcher notice can name the verification records
Engram received for that run after its association has settled durably. A
reminder about another claim is withheld only when that claim has its own
received record matching its current source and binding. These reads run
outside the shared state lock. Turn close publishes
the original correlated launcher notice without waiting for Git. The existing
cancellable capture owner confirms source in an optional observer and replaces
only that exact pending fragment; the shared provider reader does not wait for
the source check. If source confirmation
expires or cannot complete, the notice distinguishes the historical receipt
from unconfirmed current input, grants no credit, and asks for no rerun.
Changed source and invalid receipts remain separate refusal cases. Late
receipt presentation runs after durability settlement, within the caller's
remaining time and the notice cap; it cannot consume or extend the settlement
deadline. An expired or failed durability settlement leaves the notice available
for a later successful settlement to update. A failed optional held-claim read
without any received-record candidate remains silent. Ordinary checks still
supply no registered verification credit.

The ordinary handoff prepares its opening source-root guidance locally.
It also checks the local `.engram-project` declaration before composing
runtime MCP configuration, so adding or removing the file takes effect on
that prompt without depending on a background context read.
Direct sends and queue drains, including Resume and queued mailbox wakes,
use already available local context and never wait for an Engram context read
or another refresh owner before provider enqueue. When context is missing,
an existing lifetime-owned observer prepares it for a later prompt. Only one
refresh owns each session generation, and failed or slow reads leave the
undelivered nudge pending. A superseded read cannot publish into a newer
generation; cached context is consumed only after runtime delivery accepts it.
This local acknowledgement does not acknowledge an Engram delivery page.

The CLI observer retains its own six-second read deadline and child cleanup;
shutdown may wait for them. An explicit Claude compact hook keeps its separate
generation ticket and can return only the context that ticket read. Neither
path grants turn admission or test credit. See the host adapter's
[start and post-compaction context](engram-host-adapter.md#start-and-post-compaction-context).

## What it never does

- It never asks Engram for Evaluate/Begin/grant authorization to deliver an
  ordinary prompt. Advisory context reads grant no turn admission.
- It never replays or settles control requests, grants or checkpoint debts
  that earlier builds recorded, before turn gating was removed. They stay
  readable as stored
  ([records from earlier builds](engram-host-adapter.md#records-from-earlier-builds)).
- An observation is a host report, not a verdict: a check's observed result
  says what the host saw, and a check whose outcome the host cannot read is
  reported as indeterminate, not as passed. A check in a turn's report earns
  no test credit by itself; credit comes only from registered verification.
- It sends no environment or toolchain evidence, and it no longer binds one
  full gate to the other items of a batch
  ([batch full gates](batch-full-gates.md)).
- It compares a turn's source with the previous traced turn's recorded closing
  sighting, under the exact same binding, source root, root generation and root
  state. A compatible checkpoint an earlier build recorded serves once in that
  place; no older checkpoint is borrowed after a refused, undelivered or late
  sighting or a rebind. Without a compatible checkpoint, a turn containing only
  completed, non-overlapped registered Git-input checks that were clean before and after
  can use those checks' existing source measurements. Every check's start and
  after measurement is compared with the close, under the same claim, fence,
  workspace and root generation; a difference reports `content_comparison`.
  Equal measurements mean **the final input was checked**, not that every
  change since the claim was observed. The preceding interval remains unknown.
  Missing, incomplete, ambiguous or failed evidence, and unregistered or legacy
  checks without a compatible checkpoint, retain `assumed_missing_baseline`.
  This takes no new source measurement and never waits for a capture to finish.
  A turn with uncertain source attribution reports no source change and leaves
  no baseline.
- If a turn checks revision A, changes source, then checks revision B, its
  closing comparison still includes A. That report is history. For a
  registered committed-input check, Engram decides from Git's read of the
  named root, so the passing check on B counts while the root stays clean at B
  ([one rule](#one-rule-for-a-registered-committed-input-check)).
- It does not promise that a turn in which the agent takes or changes a claim
  is reported or credited under it. A turn is traced only if its project has a
  verified store or its session already has a work binding when it starts. The
  claim is read when the turn closes, a claim the session was already
  associated with must still match, and a turn with no claim reports nothing.

## Evidence

The behaviour above is covered by `src/tests/engram_all_harness_ordinary_turns.rs`:

- `<harness>_ordinary_turn_succeeds_without_gating`, `_fails_without_gating`,
  `_is_cancelled_without_gating` and `_ends_unknown_without_gating`, for each of
  the six harnesses: an ordinary turn is delivered with no per-turn request to
  Engram, and its end reaches Engram as one observation carrying that outcome;
- `<harness>_ordinary_turn_reports_its_test_and_edit`, for each harness: a turn
  that runs a test and edits a tracked file reports exactly that test, with
  its kind, result and finish time, and the source change;
- `every_harness_reports_its_ordinary_turn_in_one_shared_shape`.

Codex, Claude and Kimi report a passing test as `passed`. Cursor, Gemini and
OpenCode report it as `indeterminate` unless their tool update carries the
command's exit code (`rawOutput.exitCode`), which the tests' updates do not.

`src/tests/off_disables_gated_control.rs` covers the queue: a row whose
provider delivery is unknown stays in place while eligible rows drain in FIFO
order, mailbox wakes and human sends dispatch directly, and records from
earlier builds are kept, never replayed.

## Related

- [Architecture](../architecture.md): prompt and Resume endpoint contracts.
- [Host architecture](host-architecture.md): the design brief and its
  historical state inventories.
- [Engram host adapter](engram-host-adapter.md): the full host contract.
- [Supervised pilot onboarding](supervised-pilot-onboarding.md): the settings
  for a colleague's machine.
- [Project status](project-status.md): current limitations.
