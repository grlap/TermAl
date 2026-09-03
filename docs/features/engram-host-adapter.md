# Engram Host Adapter

## Status and product tiers

The local Engram integration has two independent tiers:

- **Base** is available to every enabled, repository-declared local project.
  TermAl injects the Engram MCP server into each local agent session and adds
  fresh `engram work next` context at session start and after compaction.
- **Turn-gated control** is a premium project opt-in and defaults off. When it
  is enabled, the existing host-private bind/evaluate/begin/checkpoint protocol
  can withhold a prompt until Engram authorizes that exact turn.

Remote proxy sessions never enter either local tier. The global
`TERMAL_ENGRAM_DISABLED` kill switch and the per-project Enabled switch stop
both MCP and context injection; turning premium control off leaves Base intact.
Project-scoped remote access remains a separate contract in
[Project-scoped remotes](./project-scoped-remotes.md).

## Configuration authority

The repository declares the project and the host supplies non-secret runtime
context:

- A local repository is declared only while its root contains a non-empty
  `.engram-project` file. Teams normally commit this marker so declaration is
  consistent across machines; TermAl validates the file, not Git index state.
- The host stores one machine-wide `binaryPath`, `home`, and
  `bootRecoveryBudgetMs`. The binary defaults to `engram` on the server
  process `PATH`; home defaults to the server user's `.engram` directory.
- Each project stores `enabled` and the default-off `turnGatedControl` flag.

There is no TermAl authority-grant setting, grant file, grant environment
variable, or grant-installing verification step. Existing persisted grant
fields from development builds are ignored and are not written again.

## Settings and verification

Open **Settings > Engram** to configure the host-global binary, home, and boot
recovery budget. Declared local projects expose **Engram settings** with:

- the Base Enabled/Disable switch;
- a separate **Turn-gated control** checkbox;
- **Verify**, which is non-mutating; and
- **Save & enable**.

`POST /api/projects/{id}/engram/verify` and the final
`PATCH /api/projects/{id}/engram` both run:

```text
engram --project-file <project>/.engram-project --home <home> doctor --json
```

Doctor must report a healthy store, a non-empty project id, and an absolute
database path. Only the premium toggle additionally requires
`control.required_assurance == "turn_gated"`. Base mode does not require or
install mutation authority.

## Base MCP composition

Every eligible local session receives an `engram` MCP stdio descriptor. TermAl
invokes:

```text
engram --project-file <project>/.engram-project --home <home> \
  mcp --actor-id termal --session-id <termal-session-id>
```

The child environment contains exactly the host context Engram also exposes to
its shell words:

```text
ENGRAM_HOME=<home>
ENGRAM_ACTOR_ID=termal
ENGRAM_SESSION_ID=<termal-session-id>
```

No authority credential is placed in argv, environment, MCP JSON, state
snapshots, logs, or private Claude MCP files. The same three values are also
available to commands run from the agent session: Claude and ACP receive them
on their per-session process, while the shared Codex app server receives no
process-global Engram identity and applies them through the thread-scoped
`shell_environment_policy.set` on both `thread/start` and `thread/resume`.
On each runtime spawn, disabled, undeclared, remote, and globally killed
projects explicitly remove inherited `ENGRAM_*` identity from per-session agent
processes. The shared Codex process is always scrubbed; when a Codex thread is
not eligible, TermAl emits no thread-level override and leaves any explicit
user-authored Codex shell policy intact. Settings changes mark an existing
runtime for the reset described under **Settings transitions**; they do not
rewrite the environment of an already-running process in place.

## Start and post-compaction context

Before the first prompt in a TermAl process, and again after an observed Codex
`thread/compacted` event or Claude stream-json `compact_boundary`, TermAl runs
off-lock:

```text
engram --project-file <project>/.engram-project --home <home> \
  work next --context-generation termal-<generation>
```

The command receives the same three `ENGRAM_*` environment values as the MCP
child. Its trimmed text is capped at 32 KiB, escapes the host fence terminator,
is wrapped in an `<engram-work-context>` block, and is prepended only to the
runtime prompt; the user's durable message remains unchanged. The cold-start
command uses the bounded Engram CLI/store-open budget (six seconds in
production), rather than the shorter control-frame deadline. A failure is
logged, does not block the user's turn, and leaves the nudge pending for a later
prompt. Concurrent prompt admission waits for the owning refresh; the context
is consumed only after the runtime command channel accepts the prompt, so spawn
or delivery failure preserves it for retry. ACP runtimes receive Base MCP, but
this cut does not yet expose a portable ACP compaction event, so their refresh
is session-start only.

## Premium turn lifecycle

Only a project with both `enabled` and `turnGatedControl` enters the control
path. For each eligible turn TermAl:

1. binds or refreshes the exact session with `assurance: "turn_gated"`;
2. calls `turn_evaluate` for the stable prompt fingerprint;
3. calls `turn_begin` for a returned grant and delivery tokens;
4. delivers the prompt only after the matching begin receipt; and
5. checkpoints the begun turn on completion, Stop, failure, reset, or deletion.

Refuse, defer, protocol/transport degradation, missing binding, begin refusal,
or dispatch-budget exhaustion withhold the prompt and produce a durable Engram
control card. Turning the premium flag off fences the transition, checkpoints
open control state, clears the binding, and resumes ordinary Base-only
dispatch.

## Human obligation waiver

`POST /api/sessions/{id}/engram/obligations/waive` is a host-private operator
action for an idle premium session already bound to the obligation's live
WorkRun. A live turn retains exclusive ownership of its checkpoint lifecycle,
so TermAl rejects a waiver while that turn, its grant, or Stop is active.
The request contains the obligation UUID, expected definition hash, displayed
human `waivedBy` identity, redactor-inspected reason, and idempotency key.
TermAl resolves the routing token and sends the strict cut-B frame:

```text
obligation_waive(routing_token, obligation_id, expected_definition,
                 waived_by, reason, idempotency_key)
```

The removed `authority_grant` field is never sent. Exact replay returns the
same typed decision; a changed intent under one key surfaces
`control_operation_idempotency_conflict`. Policy refusals are successful typed
responses (`waiver_not_admitted`, `obligation_not_open`, or
`definition_changed`) and retain Engram's remedy. Waived receipts expose the
human attribution but omit the reason.

This cut exposes the waiver as an API-only operator action; the settings UI does
not yet provide a waiver form. Like the rest of TermAl's unauthenticated local
API, this is a trusted-operator surface rather than an isolation boundary from
processes running as the same OS user. Every successful waiver therefore also
adds an idempotent durable audit card to the session transcript. The route holds
the same project lifecycle fence as settings transitions, resumes prompts
parked behind that fence when it releases, honors the adapter circuit breaker,
and rejects any receipt or refusal that does not correlate to the submitted
obligation.

## Premium boot recovery and lazy retry

Boot recovery applies only to premium control sessions. TermAl publishes
`engramBootRecoveryPending` before recovering bindings, bounds the overall
work by `bootRecoveryBudgetMs`, and retries an unfinished target lazily on the
next targeted read or prompt. Base MCP/context injection does not bind a
control session and never withholds delivery.

Recovery diagnostics keep the stable single-line form
`boot-recovery session=<id> command=<phase> attempt=<n> elapsed_ms=<n>
outcome=<ok|error>`. Phases include `session_status`, `turn_checkpoint`, the
work-focus reads, `session_bind`, and the whole target. The coordinator emits
an `overall` line with elapsed time, budget, outcome, and unfinished count.
These diagnostics contain no routing token or other host-private control data.

## Settings transitions

An Engram settings transaction owns a project-generation fence while the old
premium connection drains. Prompts arriving during the transition remain in
the durable queue and cannot bypass the committed tier. When the exact owner
releases the fence, TermAl drains the queue against the new settings:

- with premium still enabled, the prompt receives a fresh
  bind/evaluate/begin sequence;
- with only premium disabled, Base MCP/context remains active and ordinary
  delivery resumes; and
- with Engram disabled, neither Base nor premium work is composed.

Binary/home changes and project deletion retain the same generation-fenced
runtime teardown and checkpoint ordering as other premium transitions.
Affected local runtimes are marked for reset so the next turn spawns the agent
process with the new base-tier identity instead of rewriting a live process
environment in place.

## Mailbox and Stop behavior

Mailbox wakes use the same premium gate as user prompts. If a mailbox turn is
stopped or fails after acceptance, TermAl restores the exact delivered-through
sequence before successor work. A wake refused by Engram is never sent to the
agent runtime. The public Stop route remains asynchronous for a live local
turn: runtime interruption and the premium checkpoint finish on the background
owner, while repeated Stop calls remain idempotent. Persisted `Stopping` is
classified as an interrupted turn during restart recovery.

See [Agent mailboxes](./agent-mailboxes.md) for the durable delivery and
recovery contract.

## Security and failure policy

All external Engram work runs without the global state mutex. Control calls and
context nudges are deadline-bounded, JSON control replies are typed, routing
tokens stay host-private, and TermAl never opens Engram's SQLite database
directly.

## Operator verification

Before enabling a repository, verify the same binary, marker, and home TermAl
will use:

```text
engram --project-file <project>/.engram-project --home <home> doctor --json
```

Save Base settings through the project UI/API. If premium control is enabled,
one live smoke turn must show the complete sequence:

```text
session_bind(turn_gated) -> turn_evaluate(grant) -> turn_begin(begin) \
  -> agent delivery -> turn_checkpoint(checkpointed)
```

A fixture-only test, direct SQLite inspection, or control card without runtime
delivery is not an end-to-end proof. Installing a new Engram build or switching
the live host remains an explicit operator action outside settings validation.
