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
  A test-launcher run started with `--input commit` in that worktree is
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
  attribution is certain, TermAl also reports a measured sighting of the
  claim's source at its exact revision. A sighting tells Engram how fresh the
  source is; it earns no test credit and records no change.

## Delivery and context preparation

The ordinary handoff prepares its opening source-root guidance locally and
performs no Engram read. There is a current exception
earlier on queue drains, including Resume and queued mailbox wakes:
[`start_next_queued_turn_inner_off_lock`](../../src/turn_dispatch.rs) calls
[`prepare_engram_context_nudge_off_lock`](../../src/engram_mcp_config.rs)
synchronously to prepare the base-tier work context. Its production command
deadline is six seconds; waiting for an existing refresh owner has a
seven-second deadline. This preparation can delay provider enqueue by about
seven seconds under those bounds. Failure is advisory: the prompt still
proceeds, with context refresh left pending. The bounds apply to a
preparation, not a promise that every drain finishes within seven seconds.

Moving this remaining synchronous context-nudge preparation off queue drains
is open follow-up work. It is not part of the completed removal of per-turn
authorization; see the host adapter's
[start and post-compaction context](engram-host-adapter.md#start-and-post-compaction-context).

## What it never does

- It never asks Engram for Evaluate/Begin/grant authorization to deliver an
  ordinary prompt. The context preparation exception above is a work-context
  read, not turn admission.
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
- It does not compare one turn's source with the previous turn's. A turn's
  source change is measured against a compatible checkpoint an earlier build
  recorded, when one exists, and is otherwise reported as
  `assumed_missing_baseline`.
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
