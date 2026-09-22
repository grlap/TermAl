# Engram Host Adapter

## Status and product tiers

The local Engram integration has two independent tiers:

- **Base** is available to every enabled, repository-declared local project.
  TermAl injects the Engram MCP server into each local agent session and adds
  advisory `engram work next --peek` context at session start and after compaction.
- **Turn-gated control** is a premium project opt-in and defaults off. When it
  is enabled, the existing host-private bind/evaluate/begin/checkpoint protocol
  can withhold a prompt until Engram authorizes that exact turn.

Premium admission covers both ordinary root sessions and delegation children,
including direct sends and queued user, mailbox, and orchestrator turns. A root
uses its session-shaped binding; a child retains its delegation-shaped effects
and cannot fall back to root authority if its delegation metadata is missing.
Evaluate grants do not dispatch a provider prompt until begin succeeds, and
successful completion checkpoints the begun grant. Refusal, deferral, unavailable
binding, and failed recovery withhold the prompt rather than bypassing control.
Base-only and disabled projects retain ordinary ungated dispatch.
Queue promotion rechecks the evaluated intent fingerprint, not just the queue
entry ID: mailbox coalescing may change its sequence while evaluation is off-lock.
An obsolete issued grant is repaired before fresh admission. Stop's successor
path likewise leaves a changed head queued instead of using the old grant.

Remote proxy sessions never enter either local tier. The global
`TERMAL_ENGRAM_DISABLED` kill switch and the per-project Enabled switch stop
both MCP and context injection; turning premium control off leaves Base intact.
Project-scoped remote access remains a separate contract in
[Project-scoped remotes](./project-scoped-remotes.md).

## Configuration authority

The [Work visualizer](work-visualizer.md) reads the store this authority
established (binary, home and validated store identity in the project
settings) as its own host reader, independent of any agent session binding;
opening that panel never enables either tier.

The repository declares the project and the host supplies non-secret runtime
context:

- A local repository is declared only while its root contains a non-empty
  `.engram-project` file. Teams normally commit this marker so declaration is
  consistent across machines; TermAl validates the file, not Git index state.
- The host stores one machine-wide `developerName`, `binaryPath`, `home`, and
  `bootRecoveryBudgetMs`. The developer name defaults to `dev`, is normalized
  to lowercase, and accepts only ASCII letters, digits, `.`, `_`, and `-`.
  The binary defaults to `engram` on the server process `PATH`; home defaults
  to the server user's `.engram` directory.
- Each project stores `enabled` and the default-off `turnGatedControl` flag.

## Settings and verification

Open **Settings > Engram** to configure the host-global developer principal,
binary, home, and boot recovery budget. Declared local projects expose
**Engram settings** with:

- the Base Enabled/Disable switch;
- a separate **Turn-gated control** checkbox;
- **Verify**, which checks scoped readiness without changing settings;
- **Save & enable**, which repeats readiness before persistence; and
- **Run Full Audit**, an explicit, separate whole-store doctor action.

`POST /api/projects/{id}/engram/verify` and the final
`PATCH /api/projects/{id}/engram` both run:

```text
engram --project-file <project>/.engram-project --home <home> readiness --json
```

Readiness must return an exit-0 v1 receipt with `scope: readiness`, `ready: true`,
`full_audit: not_run`, and `mutation_enabled: false`. The host compares the
project id with `.engram-project` and the canonical database with the expected
SHA-256 project path under the selected home. Matched stored/resolved host-path
policy is required; unresolved or unbound reads do not authorize enablement.
Verify/Save have a ten-second process budget; unsupported older binaries,
malformed receipts, nonzero exits and timeouts fail closed, never falling back
to doctor or silently initializing/repairing a store. Engram may probe the
checkout with a temporary file to resolve filesystem identity.

Only the premium toggle requires `control.required_assurance == "turn_gated"`.
Base access accepts advisory or turn-gated stores but remains explicitly
advisory/unmediated: it does not satisfy a turn-gated control floor. Unknown or
action-gated requirements are refused. There is no authority-grant setting, grant file,
grant environment variable, or grant-installing verification step; obsolete
persisted grant fields from development builds are ignored and are not written
again.

`POST /api/projects/{id}/engram/audit` runs `doctor --json` only after the
operator chooses Full Audit. The UI shows elapsed progress, a bounded report
preview and stderr disclosures, and retains that audit result across readiness checks.
The API returns `reportPreview` (at most 16 KiB of UTF-8 text before JSON
escaping) and `reportTruncated`, not the entire parsed report object. This
display-only prefix may be incomplete JSON; health and store identity are
validated against the complete captured receipt before projecting the preview.
Warnings and each error-output excerpt are limited to 4 KiB, including an
explicit truncation marker. The UI also clamps previews defensively, renders
report text only when expanded, and does not serialize it on progress ticks.
Omitted output is not copied into logs; these previews provide no redaction.
Audits open the store writable and may perform SQLite recovery; they do not
save TermAl settings or silently retry readiness. Readiness is not a full
health pass. Quiet readiness stderr proves neither redaction nor enforcement:
the development redactor provides no secret/PII protection, and action gating,
organizational-authority mediation and action-outcome tracking are unavailable.

The router admits two nonwaiting readiness checks and one separate Full Audit;
busy pools return 429 without starting a process. Disable bypasses readiness
admission. The audit POST requires same-origin Fetch Metadata and
`X-TermAl-Operator-Action: engram-full-audit` as browser intent, not authentication
against privileged local programs. An aborted browser request does not cancel
the bounded server audit. See the [architecture route table](../architecture.md#http-api).

Doctor execution and stdout/stderr collection share one five-minute deadline
after process setup. A direct child exiting does not end output collection:
descendants may retain its pipes. Missing EOF at the deadline is an explicit
collection failure, not malformed JSON or a healthy result. Cleanup is
best-effort and reaping does not block the response. Windows uses the owned job
handle; after reaping on Unix, TermAl does not signal a potentially reused
process-group ID. Surviving descendants can retain detached reader threads
until their pipes close.

Each doctor stream has a separate 8 MiB host capture budget, independent of
control-protocol frame limits. This is a resource policy, not a measured maximum
valid report size: a valid larger report is refused with a budget diagnostic.
Partial transport captures are never parsed or accepted. Separately, readiness
receipts have a 16 KiB admission limit and fail closed above it without a doctor
fallback. These admission and capture limits are distinct from the smaller
display previews described above.

## Base MCP composition

Every eligible local session receives an `engram` MCP stdio descriptor. TermAl
invokes:

```text
engram --project-file <project>/.engram-project --home <home> \
  mcp --actor-id <developer>/<agent-kind> \
  --actor-context 'agent=<kind>;model=<model>;reasoning=<effort>' \
  --session-id <termal-session-id>
```

The child environment contains exactly the host context Engram also exposes to
its shell words:

```text
ENGRAM_HOME=<home>
ENGRAM_ACTOR_ID=<developer>/<agent-kind>
ENGRAM_ACTOR_CONTEXT=agent=<kind>;model=<model>;reasoning=<effort>
ENGRAM_SESSION_ID=<termal-session-id>
```

`ENGRAM_ACTOR_ID` is the principal: the developer's own seat is the bare host
`developerName`, while hosted agents append the lowercase TermAl agent kind
(`codex`, `claude`, `cursor`, `gemini`, or `opencode`). Session display names
and model aliases are never identity inputs. Engram matches actor ids exactly:
the bare developer and every `<developer>/<agent-kind>` seat are distinct, and
all hosted agent operations (including premium control) use the suffixed seat.
Exact agent kind, model id, and reasoning selection are rendered separately in
optional free-text `ENGRAM_ACTOR_CONTEXT`; empty or unknown detail keys are
omitted. TermAl collapses control characters and caps this context at 200 bytes,
below Engram's 256-byte limit. `%`, `;`, and `=` inside values are encoded as
`%25`, `%3B`, and `%3D`, so they cannot impersonate field separators. Any field
that does not fit is omitted whole; later, smaller fields may still be appended.

No credential is placed in argv, environment, MCP JSON, state snapshots, logs,
or private Claude MCP files. The same required identity values and optional
actor context are also available to commands run from the agent session: Claude
and ACP receive them on their per-session process, while the shared Codex app
server receives no process-global Engram identity and applies them through the
thread-scoped `shell_environment_policy.set` on both `thread/start` and
`thread/resume`.
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
  work --actor-id <developer>/<agent-kind> \
  --session-id <termal-session-id> \
  --actor-context 'agent=<kind>;model=<model>;reasoning=<effort>' next --peek \
  --context-generation termal-<generation>
```

Both startup and post-compaction use the same non-advancing orientation read:
either nudge may be truncated or never reach a runtime, so neither may consume
ordinary delivery. The installed CLI's `work next --help` defines `--peek` as
not staging or advancing delivery, focus, or memory advertisement. TermAl passes
`--context-generation` alongside `--peek` so Engram can calculate the read-only
`memories.changed` signal for the current context generation. Peek returns a
memory advertisement (`count` and `changed`), not full memory contents; those
are read through `work memories`. Repeated peeks with a new generation can keep
reporting `changed: true` without persisting or acknowledging that advertisement.
Only an advancing `next` with that generation acknowledges it; acknowledgement
is not proof that the agent read the memories. The host also uses generation
locally to reject stale nudge results and order deferred refreshes. The agent
continues ordinary advancing `work next` through MCP; host orientation does not
replace that read.
This protects the delivery cursor, not the completeness of recovery: the host
prepares the nudge at the next TermAl prompt dispatch, not synchronously at the
compaction boundary. It does not guarantee an Engram block before the model's
first action in an automatically continued post-compaction turn.

The command receives the same `ENGRAM_*` environment values as the MCP child,
and its actor/context flags are byte-identical to that environment. Its trimmed
text is capped at 32 KiB, escapes the host fence terminator,
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

Compaction requests a deferred refresh. A fetched but unsent orientation snapshot
remains available for retry; an in-flight peek finishes without discarding its
result. Only after the runtime accepts that snapshot can the next prompt fetch
fresh orientation. The retained cache and its delivery acknowledgement concern
only local prompt admission, not an Engram delivery page or receipt. Truncation,
discarding the local cache, and a prompt that is never sent leave ordinary Engram
delivery untouched; acknowledging the local cache sends no Engram command.
This applies to Claude boundaries and both Codex compaction event forms. Current
Codex item completions are deduplicated by the last 64 item ids of at most 256
bytes each; missing, oversized, or evicted ids may signal again. Deduplication is
best effort and is not the mechanism that protects ordinary delivery. Configuration
changes retain their separate invalidation behavior. These rules preserve host
handoff; they do not prove that a model read or obeyed the delivered context.

For a **new Codex thread** with Engram enabled, TermAl also reads the effective
configuration through the same app-server's `config/read`, using the thread's
working directory. It preserves existing developer instructions verbatim and
appends a short recovery instruction in `thread/start.developerInstructions`.
The read is asynchronous and has a fixed 30-second response deadline, independent
of sibling stdout activity. An error fails the requesting turn visibly and permits
retry instead of starting with incomplete instructions. Expiry of this short
deadline does not retire the shared runtime. Other sessions continue.
The read and start observe a configuration snapshot, not an atomic transaction
against concurrent edits to configuration files.

The recovery instruction asks the agent, at start and after its context is
replaced by a summary, to use Engram `next` with `peek: true`, enumerate current
project memories (following pagination), and read relevant full records regardless
of `memories.changed`, including after compaction. These are read-only recovery
operations; they do not advance ordinary delivery. Retrieved records retain their original
authority. Codex reconstructs developer instructions during compaction, but
delivery of an instruction does not prove that an agent followed it: a real
post-compaction recovery remains a separate acceptance observation.

This bootstrap applies only to new threads. Engram-disabled sessions and
`thread/resume` retain their existing setup behavior; a live resumed Codex thread
does not accept a replacement developer-instruction field. This change does not
establish equivalent intra-turn recovery for Claude or ACP runtimes.

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

### Authorization timeout and retained prompts

Ordinary gated admission uses one ten-second remaining-time budget across work
focus, binding, evaluation, begin, and host persistence acknowledgements. Base
context reads remain a separate operation. A timeout or unavailable transport
does not mean policy denial: the session shows **Waiting/Unknown**, pauses its
queue, and keeps the original prompt, attachments, source and identifier. Resume
retries unknown authorization; Cancel and Stop remain available without waiting
for the Engram call. An explicit Refuse still withholds delivery as a refusal.

If a retained prompt already appears in the visible transcript, a paused queue
shows an action-only recovery card without repeating the prompt body. When the
prompt was never promoted or its transcript row is outside the resident window,
the recovery card also shows the original prompt content and context. Retryable held
authorization offers Resume and Cancel; a pending prompt projected with
`engramInterrupted: true` offers Cancel/reconciliation guidance, not a no-op
Resume. Removing it preserves the operator's intentional queue pause.
A Stop-marked head remains ahead of later mailbox and user
prompts even when Stop arrived before the first authorization request was saved.
An unresolved Stop defers an already-admitted handoff in the Stop callback queue.
Failed Stop replays the exact delivery only while the original runtime, turn and
authorization still own it; successful Stop or replacement discards it.
The public asynchronous Stop restores that admitted turn before replaying a
failed shutdown's handoff, while still publishing the Stop failure. Successful
rollback also restores the exact admitted owner when the handoff has not yet
arrived; callback arrival order is not evidence of admission. Successful
Stop retires its promoted queue head even after the durable begin receipt has
consumed the pending dispatch marker; Resume can start a successor, not replay
the stopped authorization.
Definite supersession is a no-op, not a channel failure. Retryable authorization
is parked before terminal delegation/orchestrator failure handling, so a child
or follow-up keeps its original prompt. An immutable mailbox head also covers
its original wake boundary: recovery does not insert a second copy of that wake,
while genuinely newer inbound sequences remain separate.

The default timeout for an individual control call is also ten seconds (or the
configured project call timeout). Outside admission this bounds each completion,
project-reset or stale-begin checkpoint and obligation waiver independently;
these calls do not share the admission deadline. A completion checkpoint can
therefore delay the next queued turn by that call timeout. Admission itself still
uses one shared ten-second budget, not ten seconds per step.

Prepared bind and evaluation requests live on the existing queued prompt. They
are acknowledged by the host persistence writer before transmission, retain
their original store/principal, and replay with the same payload and idempotency
key after a lost reply. A received Defer completes that evaluation; explicit
Resume asks a new operation for the same retained prompt. Authority-relevant
settings changes do not silently authorize replay under another principal;
an incompatible retained bind/evaluation stays paused and explicitly interrupted
for cancellation or reconciliation, including in delegated children;
completion-evaluator defaults do not invalidate an otherwise identical retry.
Public settings resets preserve that barrier even when they clear the transient
recovery flag. Refusal (or control disabled before an operation) retires the
failed head. Other degraded outcomes with retained intent, including protocol,
store and local persistence faults, remain interrupted and cancelable; they are
not silently retried or allowed to terminalize a waiting child. Transient
transport failures and Defer remain explicitly retryable.

Queue records persist their original promoted transcript position. Trimming the
resident transcript cannot cause a retry to append the same prompt or composer
history again. Older retained records recover the marker from known transcript
positions; absent historical evidence requires explicit reconciliation.

On restart, recovery checks the original control session before rebinding.
Engram expires issued-but-unbegun grants when its control connection restarts:
the host reconciles an obsolete replayed grant before requesting a fresh one.
A **begun** grant has unknown provider delivery after host restart. Its begin
acknowledgment is persisted on the retained evaluation before provider handoff,
so a later closed remote grant cannot erase possibly-delivered evidence during
an asynchronous queue-removal write. Recovery persists the interrupted state
before checkpointing a still-open grant. The prompt remains held for explicit
resolution, never automatically resent. If control is off after a restart, the
retained authorization is surfaced for cancellation rather than silently
blocking or sending it without admission. This is at-most-once host handoff,
not a claim of exactly-once model execution.

Opt-in tests in `src/tests/engram_root_recovery_live.rs` use a caller-identified
Engram binary (`TERMAL_TEST_LIVE_ENGRAM_BINARY` and its SHA-256 in
`TERMAL_TEST_LIVE_ENGRAM_SHA256`), disposable stores and a simulated provider
receiver. They exercise real control replies, committed-reply loss, restart,
deadline, explicit refusal and writer contention without touching live stores.

### Strict Save and absent control sessions

A strict settings Save still refuses uncertain checkpoint failures. The only
absence recovery is an exact `control_session_not_bound` refusal for an errored
session with no attached runtime, while TermAl owns its project-reset and
checkpoint fences. That error alone is **not** proof: Engram also uses it for a
binding belonging to another project, and `session_status` is not read-only.

TermAl asks the old configured binary/store for a separate read-only snapshot:

```text
engram --project-file <marker> --home <home> control-session-inspect \
  --target-session-id=<session> --retained-grant-id=<grant> --json
```

Only an exit-0 v1 `control_session_inspect` receipt with mutation disabled,
matched host-path policy, exact project/canonical database/session/grant identity,
and all three presence flags false is admitted. The producer checks the session
row, **all** grant rows for that session, and the retained grant anywhere in the
store, including project mismatches and orphan rows. Missing/older commands,
malformed or oversized receipts, nonzero exits and unresolved identity refuse
recovery. A Save shares one ten-second inspection deadline, starting at the
first eligible probe. Every later probe, pipe collection and evidence admission
uses that same deadline; exhausted budgets refuse the Save and retain authority.
The 16 KiB receipt limit applies per probe. This is not a bound on the whole
Save: checkpoint RPCs before the first probe and post-commit rebinding have
their own budgets. OS filesystem calls are not cancellable, so they can return
late, but late evidence is refused and no further probe is launched.

On Windows, absence inspection refuses configured `.cmd`/`.bat` wrappers before
launch: their `cmd.exe` argument reparsing cannot safely transport arbitrary
producer selectors. Configure the native Engram executable instead. Native
executables and supported PowerShell scripts keep the producer selector grammar;
selectors use `--flag=value` to keep leading hyphens literal. Other diagnostic
commands and the disable/authority-retirement escape paths are unchanged.

The receipt is snapshot evidence, not a store lock or authority to repair it.
TermAl checks the old project/canonical-path routing identity before and after
the probe and again off-lock just before acquiring the commit lock. Under that
lock it rechecks the persisted identity, reset ownership, exact
connection/token/grant and session generations using only in-memory state.
Filesystem reads and canonicalization never run under that final state lock.
These observations do not lock external filesystem routing or authenticate a
copied/replaced database at the same path. Only the atomic settings Save clears
the stale local state; rollback retains it and a retry requires fresh evidence.
No session deletion,
policy-floor reduction, rebinding or grant expiration is part of inspection.
This does not cover active/attached sessions or arbitrary checkpoint refusals.
See the [settings API](../architecture.md#http-api).

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

Exact replay returns the same typed decision; a changed intent under one key
surfaces `control_operation_idempotency_conflict`. Policy refusals are
successful typed responses (`waiver_not_admitted`, `obligation_not_open`, or
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

## Acceptance evaluation

An Engram project whose policy evaluates acceptance refuses `done` until a
per-criterion evaluation is recorded. TermAl produces that evaluation for the
Base tier and up; it needs no premium control session. The delegation side is
described under
[evaluator delegations](agent-delegation-sessions.md#evaluator-delegations).

**Two routes, two callers.** The paths differ by one letter on purpose and are
not interchangeable:

| Route | Caller | Meaning |
| --- | --- | --- |
| `POST /api/sessions/{id}/acceptance-evaluations` (plural) | the parent session whose task needs judging | a request that *may create* an evaluation: it reads the tracker and either spawns an evaluator or answers with a same-session brief. Body `{ workRef, agent?, model? }` |
| `POST /api/sessions/{id}/acceptance-evaluation` (singular) | the evaluator child itself | the *one* evaluation that child owns: `{id}` is the child, and the task, mode, bases and attempt key are the host's. Body `{ schemaVersion, verdicts }` |

The plural route is a collection the parent adds to; the singular route is the
single resource an evaluator child has. Neither accepts the other's body
(unknown fields are `422`), and a parent calling the singular route is refused
for want of evaluator authority.

**Request.** `termal_evaluate_acceptance`, or
`POST /api/sessions/{id}/acceptance-evaluations` with
`{ "workRef", "agent"?, "model"? }`. TermAl:

1. builds the [Work view](work-visualizer.md)'s host reader for the session's
   project, so both reads run under the operator-validated binary, home and
   store, and neither registers the requesting session in the tracker. A
   project without an enabled, verified integration is refused with the
   reader's reason;
2. reads the task in two calls, because the tracker's CLI refuses `--full`
   together with the evidence windows: `engram work show REF --notes --gates
   --json` for the acceptance and evidence bases, the evidence window, the
   lifecycle and the task's pinned mode when it has one, and `engram work show
   REF --full --json` for the complete title, outcome and criteria in order
   (the windowed read clips long ones). The complete contract must be at the
   revision the acceptance basis names; otherwise the task was revised between
   the reads and the request is refused. A task that is not open, has no
   criteria, or reports no bases is refused. The tracker's evidence window is
   byte-bounded, so long notes leave older evidence behind the first page; the
   host follows `notes_window.after` (at most eight pages, until the brief's
   40-entry cap) and lists everything it collected oldest first. A failed
   continuation only shortens the brief. The work ref the tracker answers
   with, not the caller's spelling, is what the host keeps and later passes to
   `evaluate`; it is held to the same rule as the caller's (non-empty, at most
   128 characters, no leading `-`, no whitespace or control character) and a
   receipt that breaks it is a `502`;
3. reads the admitted modes from `engram control-policy show`
   (`acceptance_evaluation.allowed_modes`), which reads the policy head only.
   `engram doctor --json` carries the same key but audits the whole store
   first, over a minute on a large one, so it is never the per-request read.
   An empty list is refused: that store does not evaluate acceptance. A
   missing key, or a read that fails (an older binary has no `show`) or
   exceeds its 10-second bound, leaves the set unknown and refuses nothing:
   the tracker enforces its policy when the evaluation is recorded;
4. selects the mode: the task's pin, else the project's default when explicitly admitted,
   else `independent_session` when admitted
   or unknown, else the host's next preference among the admitted modes,
   `sub_agent` then `same_session`. The order is the host's, not the order in
   which the policy lists them. A pin the policy does not admit is refused
   naming both.

`independent_session` spawns an evaluator delegation and returns the ordinary
creation response plus `mode` and `workRef`; the parent waits with
`termal_resume_after_delegations`. `same_session` spawns nothing and returns
`{ mode, workRef, acceptanceBasis, evidenceBasis, brief }`, where the brief
tells the caller to record the evaluation with its own tracker tool.
`sub_agent` returns `501`.

**Read budget.** Every tracker call runs through the one-retry lock policy, so
it can cost two command timeouts plus the retry delay. One function computes
the worst case of a request (the two task reads, up to seven continuation
pages and the policy read) and both sides use it: the MCP bridge adds it to
its HTTP allowance, and the request path takes it as its own deadline. Before
each continuation page the host checks that the deadline still funds that page
and the two reads that decide the request; when it does not, paging stops and
the brief lists the evidence read so far. The bridge therefore never gives up
on a request the backend is still serving.

**Store identity.** The bases and the work ref mean something only in the store
they were read from. The host keeps that store (the project id and database
path the operator established) on the evaluator's target. The reads run
without the state lock, so under the lock that creates the delegation the
parent's project is resolved again and the spawn is refused with `409` if its
store is no longer the one that was read. At submission the *child's* project
is resolved and the write is refused with `409` ("the project's tracker store
changed since this evaluation was requested; request a new evaluation") if its
store differs, whether the operator re-pointed the project or the child
resolves to another project than the parent did. A target persisted before the
store was kept cannot submit and asks for a new evaluation.

**One active evaluation per task.** At most one evaluator per store and work
ref is queued or running, whichever way it would become active:

- a request is refused with `409` while one exists; the refusal names that
  delegation and its parent session. The check shares the creating lock with
  the store check, so concurrent requests produce one evaluator;
- a follow-up (`termal_followup_session`) that would rearm a finished evaluator
  is refused with `409` while another evaluator of the same store and work ref
  is active, naming it. The reservation step answers early, and prompt
  admission repeats the check under the lock that rearms, so a request racing
  a follow-up still leaves exactly one. A rearmed evaluator in turn blocks a
  new request.

A finished evaluator does not block. If its write outcome is unknown (below),
the new request's answer carries a `notice` saying so: the tracker may already
hold that evaluator's verdict.

**Brief.** The evaluator's prompt is built by the host from those reads and is
never supplied by the caller. Every interpolated field is one line with control
characters removed. The acceptance criteria are the contract the verdicts
answer, so they are never truncated and never dropped. The rest is context, and
it is measured in UTF-8 bytes, the unit of the 65 536-byte prompt cap it
competes for: the outcome is kept to 16 000 bytes, cut on a character boundary
with an explicit `[outcome truncated by the host]` marker; the evidence list
keeps the newest 40 entries and states how many older ones are not shown; an
entry the tracker would refuse as a citation (a non-holder observation, a
restored-record member) is marked as context only. When the brief must shrink,
context gives way first and in this order: evidence entries, oldest first, down
to none with the outcome still at its own bound; only then the outcome, down to
its marker alone (an outcome shorter than the marker is never traded for it).
Only when the complete criteria do not fit with no context left is the request
refused, with `409` "the acceptance contract is too large to brief an
evaluator", which states the bytes the criteria take and the limit. Non-ASCII
context therefore shortens the brief; it never produces that refusal for small
criteria.

The `same_session` brief is held to the same 65 536-byte bound and the same
refusal. It carries the complete criteria and the bases and no context that
could shrink, so a contract is briefed whole or refused alike whichever mode is
selected.

**Submission.** The evaluator is read-only and cannot reach the tracker's own
`evaluate` tool, so it calls `termal_submit_acceptance_evaluation`
(`POST /api/sessions/{childId}/acceptance-evaluation`). Before any process
runs TermAl checks authority and shape: exactly one verdict per criterion;
`pass`, `fail`, `insufficient-evidence` or `needs-human`; an optional basis of
`observed`, `asserted`, `judgment` or `human-required`; a single-line
rationale of at most 2 000 characters; at most 8 evidence locators per
criterion, each 8 to 64 lowercase hex characters; and at least one locator on
every pass. TermAl then runs, as the evaluator:

```text
engram work --actor-id <child seat> --session-id <child session> [--actor-context …]
  evaluate REF --mode … --acceptance-basis N --evidence-basis M
  --verdict P=VERDICT:BASIS --rationale P=TEXT [--evidence P=LOCATOR]…
  [--model anthropic/<model> | openai/<model>] --attempt <delegation id> --json
```

The session id, seat and actor context are the child's own, the bases are the
ones read at request time, and the attempt key is the delegation id, so the
tracker replays an identical resend (its receipt says `replayed: true`) and
refuses different content once an evaluation is recorded. The command is also
bounded as a whole: an argument list past the conservative Windows command-line
limit is a `400` before anything runs.

**One outcome model.** The tracker's write and the host's record of it can come
apart (a lost response, a failed persist), so the target carries an explicit
`submission` state and the host never infers "nothing was recorded" from
silence:

| State | Meaning | How it is entered |
| --- | --- | --- |
| absent (`none`) | nothing is recorded by this evaluator | initially; and, only for the request that itself entered from `none`, after a first send with positive evidence that it recorded nothing: a tracker refusal (below), a store that stayed locked, or a tracker process that never started |
| `pending` | a write was started and its outcome is not yet known | set, with a digest of the exact argument list and that list itself, and acknowledged durable *before* the tracker runs |
| `recorded` | the tracker holds the evaluation | a success receipt, acknowledged durable before the success answer. The record keeps a bounded extract (`evaluationHash`, `mode`, `passed`, `verdictsTotal`, `blocking`, `replayed`, `workRevision`, `evaluatedCut`), never the raw receipt, which can be a whole control frame; the child's answer carries the raw receipt, cut to 16 KiB on a character boundary with `receiptTruncated: true` when it is larger |
| `unconfirmed` | the host could not learn whether the write landed | see below; its `reason` leads with the latest thing learned |

*One submission at a time.* At most one submission per evaluator delegation is
in progress, from its admission through the tracker run to its last durability
acknowledgement. The marker is taken under the state lock in the critical
section that admits the submission and released by a guard on every way out, a
panicking tracker runner included; it lives in memory only, because after a
restart no request is in flight and the persisted `pending` already carries
what one left open. A second submission meanwhile, with the same verdicts or
others, is refused with `409` "a submission for this evaluator is already in
progress; submit the same verdicts again when it has answered": it runs nothing
and changes nothing, and it is answered before the record is even read, so an
unacknowledged `recorded` in memory never decides another caller's answer.
Settlement also never moves backwards: `recorded` is final, and an open write
returns to `none` only by the request that wrote it as its own `pending`.

*Durable means acknowledged.* A commit only wakes the persistence writer, so
the host asks that writer, through the content fence it already has for
delegation records, to acknowledge that SQLite holds exactly this record, and
waits for the answer without holding the state lock. The acknowledgement names
an exact record, so a record that moved on for an unrelated reason (a cancel, a
status refresh) would never match; while the submission state in it is still
the one this request wrote, the host asks again about the record as it now
stands, at most three records within the one deadline. A submission state that
is no longer the one written ends the wait as a failure. The acknowledgement is
never skipped:

- no acknowledgement of `pending` (a failed write, a deadline, a stopped
  writer): the tracker is not run, memory returns to the prior state and the
  answer is `500` "nothing was sent". A restart can therefore never find an
  absent state beside a tracker write;
- no acknowledgement of `recorded`: memory returns to `pending`, so memory never
  says recorded while disk may not, and the answer is `500` telling the
  evaluator to submit the same verdicts again; the tracker replays them and the
  receipt is recovered without a second write;
- `unconfirmed` is acknowledged the same way; when that fails it stays in
  memory and is logged, because disk still holds the acknowledged `pending`,
  which reads the same to the parent. `none` is not waited for: a restart that
  still finds `pending` errs on the safe side.

Each wait is bounded (five seconds), and the MCP bridge's HTTP allowance for a
submission covers two tracker sends and two acknowledgements on top of its
ordinary budget.

*A refusal needs positive evidence.* The host believes that a run recorded
nothing only from a process that ended on its own with the expected exit code
*and* the known shape, both together:

- exit code `1` with stderr that parses as Engram's error envelope,
  `{"error":{"code":<word>,"message":…}}`, which Engram prints only when the
  operation's transaction did not commit; or
- exit code `2` with the argument parser's usage error (`error:` … `Usage:`),
  raised before any store is opened.

Every other ending leaves the write unknown: a process killed by a signal, a
Windows crash status (an NTSTATUS value such as `0xC0000005`), exit `101` with
a panic message, exit `1` with free text (failing to print the receipt happens
*after* the commit), an envelope with another exit code, empty or unparseable
stderr. The exit is kept as data (a code, or "did not end on its own"), never
read back from a formatted status string. A locked-store diagnostic is believed
only with Engram's ordinary failure exit code 1, never a panic or another exit
code. This write-outcome classification does not change the separate JSON-read
lock retry policy.

*Uncertainty is erased only by a receipt.*

- A deadline, a transport failure after the tracker process started, a success
  exit whose stdout cannot be read, or any failure that is not a refusal as
  defined above leaves the write unknown. The host immediately sends the
  identical argument list once more, which is replay-safe. A receipt settles
  it: `recorded`.
- A `pending` or `unconfirmed` state found at the start of a request (an
  earlier request's write is open, same verdicts) counts exactly like an
  unknown first send of this request: this request's one send is already the
  identical resend.
- *The resend is the original.* The argument list names things the host
  derives (the actor id from the developer name, the actor context, the model),
  and those can change while a write is open. So the open write keeps the exact
  argument list it was sent as, beside the digest of the whole list and a
  digest of the evaluator's own part, its normalized verdicts. A later
  submission is recognised by that part: the same verdicts run the *stored*
  list verbatim, under the actor it names, whatever the settings say now, so
  the tracker sees the identical attempt; other verdicts get the `409` below.
  The stored list is host-private: it is persisted with the record and left out
  of every status, result, list and delta the host serves. If it can no longer
  be run from this host (it cannot be read back, or no longer fits the command
  line), the answer is a `409` that says so and sends the evaluator to report,
  not to retry.
- While a send is unknown, nothing but a receipt settles it. A locked store, a
  process that never started, another unknown result *and a tracker refusal of
  the resend* all keep `unconfirmed`. A refusal then answers `409` with the
  tracker's text plus: an earlier send's outcome is unknown, so these verdicts
  cannot be changed; the evaluator finishes and reports, and the parent reads
  the task. The others answer `502` "the write outcome is unknown; submit the
  same verdicts again".
- *Why a refused resend proves nothing (the run caveat).* Engram replays an
  exact resend before any revision or lifecycle check, but the attempt identity
  is bound to the item's active (or latest) run. If the run changed between the
  two sends, the resend is not recognised as a replay and is judged as a new
  write; its refusal then says nothing about whether the first send landed. The
  host cannot see runs, so it never reads a refusal as "never landed".
- With nothing open, a run that recorded nothing leaves nothing open: a clean
  refusal is relayed as `409` and the evaluator may correct its verdicts (that
  is what lets it add a missing citation), a store that stayed locked is `502`
  "nothing was recorded", and a tracker process that never started is `502`
  "nothing was sent". Never-started is a typed signal from the process runner
  (a failed spawn), not a reading of message text.
- While the state is `pending` or `unconfirmed`, a submission with other
  verdicts is refused with `409` ("an earlier submission's outcome is unknown;
  submit exactly the same verdicts to resolve it"): the tracker may hold the
  open write, and only its replay is safe. The evaluator child stays admitted
  in both states.
- `recorded` refuses every further submission.

The parent reads the state in the status and result packets and in the fan-in
line: `none` is "nothing was recorded"; `pending` and `unconfirmed` are "the
write outcome is unknown: the tracker may hold this evaluator's verdict; read
the task before requesting another evaluation", and `unconfirmed` adds what was
last learned (a refused resend included), to be read, not acted on; `recorded`
names the time and how many criteria passed.

### Operator acceptance settings

The project's Engram settings show the current store policy using
`control-policy show`, never doctor. An empty mode list is **off / self-asserted**;
an unavailable or older binary is **unknown**, not off. Evaluator defaults are
stored separately in `engram.acceptanceEvaluation`: `defaultMode`, `evaluatorAgent`
(Claude or Codex), and `evaluatorModel`. Saving defaults does not audit the store,
reset sessions, or change policy. Task pins win; a default mode is used only when
the current policy explicitly admits it. Unsupported `sub_agent` defaults are
rejected server-side; defaults require an existing Engram configuration and do
not turn an unconfigured project into an operator veto. Explicit request agent/model win over
project defaults. With no agent preference, choose the other Claude/Codex vendor
when its readiness check is ready. A request-provided `model` requires an explicit
`agent` (otherwise HTTP 400), so Auto cannot choose a different model vendor;
without a ready alternate vendor, use the parent's agent. With no model
override the selected agent uses its normal default. A saved model override
requires a concrete evaluator agent and is applied only when that agent is
selected; an explicit request selecting another agent cannot inherit it. Model
names are trimmed on ingest. The UI clears the model when its agent changes.
See the shared [Claude launch and readiness contract](../architecture.md#claude-code),
which applies to every Claude session, not only evaluators.
Claude readiness and runtime launch share one PATH resolver. Windows requires
native `claude.exe`, not `.cmd`/`.bat` shims; Unix requires an executable `claude`.
A missing launchable CLI blocks new Claude sessions with installation guidance
(authentication remains a runtime check). GUI-launched hosts must have the CLI
directory on their PATH. Evaluator defaults are saved only by their dedicated
button: editing them does not invalidate connection verification, and saving
them does not discard an unsaved turn-gating draft (or vice versa). Switching
projects releases the old acceptance form's busy state; late completions cannot
unlock a newer project's in-flight operation.

The project picker and acceptance-setting dropdowns use the shared themed
combobox, matching the session-list menus on Windows as well as other platforms.
Unsupported evaluator modes remain visible but disabled.

Changing store policy is a separate operator action, submitted with the
"Confirm policy change" button; no additional confirmation checkbox is required.
No justification field is collected or sent; policy administration uses the reason-free Engram CLI.
The captured setter help fixture records build `e364f61f49a4` (2026-09-21);
default tests check both advertised and required flags. On 2026-09-21, isolated
real-binary tests also passed against build `d7d8caddc923`, schema `025fb9bb102f`,
SHA-256 `11c88b50b5602f226694e3da79e8ad5adde93d0c7f50690448d88084b6ccf444`:
reason-free init, acceptance-policy write and exact receipt replay, readiness
verify/save/audit, and bind/evaluate/begin/checkpoint with a fake provider.
These tests use disposable stores, not the installed runtime or live projects.
The form sends the complete replacement policy, the displayed opaque policy ID
(`expectedPolicy`), the host reader identity, and a stable idempotency key.
`control-policy set-acceptance-evaluation` uses the policy ID as its CAS guard;
other policy dimensions are not changed. No modes means off. The form retains
the exact payload/key in tab-local session storage before sending and offers
identical retry across dialog closure, project switches and page reloads in that
tab. Recovery is not shared between tabs: another tab can advance the policy
head while this tab retains an unresolved attempt. A later conflict does not
prove whether that earlier write landed; reconcile the original store before
discarding recovery data. The write slot serializes active calls, not unresolved
attempts across tabs. Closing the browser tab clears this recovery state. An unreadable saved
attempt explains that remedy and asks the operator to reconcile the store before
discarding recovery data. Unavailable storage must be restored. Storage failure blocks
a new write. Refreshing policy never replaces an open draft's original CAS base.
Beginning a policy write cancels older reads so they cannot overwrite its result.
A definitive first-attempt parser refusal leaves the form editable. There is no
fallback to the removed CLI argument or migration of old saved request payloads;
unrecognized saved payload fields fail closed using the recovery guidance above.
A first-attempt 409 reloads policy for a new explicit decision, never retries
against the new head automatically. Once an earlier attempt is uncertain, a
later 409 (including reset, reader or policy conflicts) cannot establish its
outcome: the exact payload/key remains retained for original-store recovery.
A successful command returns `writeApplied: true`, even when a following read
fails or the project settings change; an unavailable snapshot explains that the
change applied to the selected store and disables further editing until refresh.
Read and write process calls have a ten-second
bound (policy reads retain the existing lock retry). Each router has two
nonwaiting display-read slots and one separate policy-write slot. Aborted display
reads may finish their bounded CLI work, but cannot consume the write slot.
Each pool returns 429 when its own capacity is occupied.
The complete-replacement editor fails closed on unknown modes or receipt shapes,
rather than silently dropping policy fields it cannot represent. Unknown fields
inside `acceptance_evaluation` are rejected; unrelated top-level policy dimensions
are allowed because this setter does not replace them. The captured isolated-store
receipt's informational `epoch` and `required_assurance` may be absent; the CAS
policy ID and replaceable policy dimensions still must validate. The captured
`control-policy show` fixture pins the editor's schema-1 receipt contract separately
from the evaluation reader's intentionally lenient mode-only interpretation.

The policy mutation route is not exposed as an agent MCP tool. It requires the
browser's same-origin Fetch Metadata and an explicit operator-action header.
This is an intent/cross-site guard, **not authentication** against privileged
local programs that can forge HTTP headers in this local single-user product.
The defaults PATCH is an ordinary local preference endpoint, without that
browser-intent header: it changes future selection, not the store's acceptance
policy. Privileged local code can change those preferences just as it can forge
the policy headers. Independent-session isolation means a separate evaluator
execution/context, not a guaranteed different vendor; explicit same-vendor
selection is supported. Neither endpoint is an authentication boundary against
local code.

Parent evaluator cards show the task/mode and receipt-backed passed-criteria
count independently of the child transcript. Completion without a submission
is not a pass; pending/unconfirmed writes remain unknown. A recorded value still
waiting on its persistence acknowledgement is shown as submission in progress.

Not delivered yet: `sub_agent` mode with a host-attested parent and execution
identity; a source fingerprint at evaluation and completion; observed build
evidence through the control checkpoint; and Work-panel evaluation actions.

## Premium boot recovery and lazy retry

Boot recovery applies only to visible local premium control sessions with a
routing token, an active grant or a rebind marker, including roots that have
never created a delegation. Never-bound sessions are not eagerly bound or
readiness-fenced: their first turn performs normal lazy admission. TermAl publishes
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

Developer-principal, binary/home changes and project deletion retain the same
generation-fenced runtime teardown and checkpoint ordering as other premium
transitions. Host principal or path changes require every Engram project to be
disabled first.
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
