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
A root session binds and requests `observe`, `communicate` and `mutate_local`,
because it edits its own workspace; a child mediates `mutate_local` only when
its write policy lets it write (a shared or isolated worktree), so a read-only
child never requests mutation. A session keeps the set it was bound with until
its next bind, and every restart rebinds, so a changed set takes effect on the
first bind after an upgrade. A session still bound under an earlier set, such
as one whose retained bind was replayed across the upgrade, is refused with
`control_assurance_insufficient` naming an effect its declared set lacks;
when the current set includes that effect and it needs no more assurance than
the host declares, admission rebinds with the current set and re-evaluates
once, as it does for `stale_fence`, and a refusal that survives the rebind is
final. The same code's other forms, a project policy stricter than the host's
assurance or an effect that needs more assurance than the host declares, no
rebind can cure, so they are reported at once. A retained evaluate replayed
across the upgrade is granted the effects it requested, and when that grant's
begin is refused the re-evaluation asks again for the same effects, since it
runs under the same binding; only a `stale_fence` re-evaluation, which
rebinds, asks for the current set, and the session otherwise takes the
current set at its next admission. The turn's
observation is judged against that grant's effects, which Engram checks it
against, not against the session's current set, so a turn that changed
source under such a grant reports nothing.
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
Verify/Save have a twenty-second process budget; unsupported older binaries,
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

The work binding the bind carries is read off-lock under the agent's own
Engram session id, the one its MCP child uses, with `work core held`: one
read-only snapshot, which selects no focus, stages or discards no delivery
and appends nothing, listing every item the session holds a live claim on,
newest claim first, each with its `control_binding` and whether it is the
session's focus. A claim's binding is `null` when `session_bind` would
refuse it (a pending handoff offer, a run no longer claimed or active); an
absent key reads the same. A revision by the holder re-accepts its claim,
so the item then shows a fresh binding at the new revision, which the next
admission's read picks up and rebinds to. TermAl binds the
focused claim if it has a binding; otherwise the claim the session is bound
to now, while it still has one, with its current revision and fence;
otherwise the most recently claimed one that has a binding; and without
work when none has. Engram lists at most sixteen claims, newest first, with
a count of those left out: when it left some out and the bound claim is not
listed, TermAl keeps the bound claim rather than switching, and if that
claim is in fact gone, Engram refuses the binding as stale and the refusal
heal reads again. The read goes by claim, not by focus, because any agent
verb that names another item moves focus: a note on another item must not
unbind the session. A binding that is present but does not decode, or
output without the item list, is a protocol error. A control session
carries one binding, so a turn's evidence lands on that claim's run only.
This read needs an Engram build that has `work core held`
(w-ec12d89a0445); against an older build every bind fails, and the failure
says to install a newer Engram. It replaced
`work core next --sections focus` followed by `work core focus`, which
selected the focused item and so could move an
agent's focus back and discard its staged delivery page.

The binding is read at every bind, and again when a new turn is admitted
after a turn has begun since the last read. An agent claims work
mid-session, after its session was bound without work, and Engram counts a
session bound without work as always current, so nothing else would carry
the claim to Engram before a restart. Only the session's own agent takes or
releases its claim, and only during a turn, so an admission after a turn
that never began reads nothing. A read that differs from the bound binding
(a claim taken, released, revised or retaken) arms one rebind, which binds
exactly what was read; an unchanged read rebinds nothing, and each rebind is
counted in the log. The read runs off-lock inside the admission budget,
capped at two seconds; a read that fails or times out leaves the binding in
place and is logged, and the next admission reads again. The rebind a read
arms is a bind like any other: if it fails or times out, the turn is
withheld as for any bind failure, its prompt retained, rather than evaluated
under the routing token of the binding the read found outdated; resuming it
replays that rebind exactly, without reading again. An admission that
loses its queued prompt while the read runs acts on nothing it read. A
retained evaluate or bind is replayed exactly, under the binding it was
prepared with, without a read. The turn in which the agent claims cannot
carry evidence for the claim: Engram refuses a rebind while a turn is begun
and admits evidence only under the binding its grant was issued with, so
the next turn is the first that reports under the claim.

Engram validates a binding against more than the read checks (the item's
ancestors, the run's root execution, a pending handoff), so a read can offer
a binding that every bind and evaluate refuses as stale, and resending it
would refuse every turn. Every binding Engram refuses as stale (at bind,
evaluate or begin) is therefore left out of the reads for five minutes from
its own refusal: the rebind that follows binds another claim the session
holds, in the usual order (the focused one, then the one it is bound to,
then the newest), so focus moving to a refused claim does not unbind a
session from a valid one, and two refused claims cannot take turns being
sent; with no other, it binds without work, which Engram always admits, and
the turn goes ahead without evidence. A claim that moves on (a new revision
or fence) is another binding and is not held back, and after its five
minutes the same binding is tried again. At most sixteen refusals are
remembered, the oldest forgotten first. Engram's `stale_fence` always
concerns the work binding, but at begin it names the
binding the grant was issued under: a begin refused for a grant issued
before the session was rebound is that grant racing the rebind, so it is
evaluated again under the current binding and the breaker blames neither.

The checkpoint that closes a turn reports one execution observation for it:
the outcome of the transition that closed it (succeeded on completion; failed
on a failed or errored turn, an exit that reported an error, or a confirmed
failure of a matching runtime through the atomic terminalization path;
unknown for Stop, termination, reset, a silent exit, a missing runtime or a
rejected delivery); `source_changed`, reported with the `mutate_local` effect
Engram requires for it, else `observe`; the intent fingerprint the grant was
issued for as the action fingerprint; and, when the session's workdir lies in
a worktree, the canonical root of that worktree (a session in a subdirectory
shares its worktree's) with the content revision of the files present in it
as the source basis, together with the observation time (see
[Content revision](#content-revision)).
`source_changed` is decided by content: the same
fingerprint is taken before the prompt reaches the runtime and again at the
close, and a difference is a source change. When both exist, the turn's
file-change tracking is not consulted: the watcher credits the turn with any
write under the session's workdir, by any writer and inside a nested
worktree too, so it reported changes the content never showed (tm-97wp).
The tracking, a debounced hint, decides alone only when the closing basis is
missing; a turn whose begin-time basis is missing but whose closing basis
exists cannot be cleared by the comparison and is reported as a change under
a grant that mediates local mutation, the conservative answer. Each capture
runs under the reviewer's shared forty-second freeze budget, because a turn
closes at the host's busiest moment and a tighter bound would drop the basis
when Git is merely slow; at the close the capture is taken before the
checkpoint is claimed, so session teardown's settle wait for a checkpoint
still covers only the control call. Two limits are known and tracked
separately. The captures run on the thread that dispatches or closes the
turn, and for a shared Codex runtime that is the app-server's event reader,
which every Codex session on that runtime shares: a capture (typically under
a second, at most the freeze budget) delays their streamed events for as
long as it runs, on top of the control call that already ran there. On the
teardown paths (Stop, termination, reset, revocation) the capture is taken
while the runtime may still be running and before it is shut down, so a
write the runtime makes between the capture and its shutdown is not
attributed to the turn. The comparison is also worktree-scoped: an edit
another session makes in the same worktree during the turn counts as this
turn's change, which withholds a
read-only child's observation and, where mutation is granted, attributes the
edit to the turn; over-reporting is the conservative direction, but children
sharing a busy workspace will often report nothing. That fingerprint is what a
later host-run check reports as its revision, so the two compare equal
exactly when nothing changed in between. The first closing attempt builds
the report and the record keeps it for that grant; a retried checkpoint
repeats it verbatim, and the observation is folded into the idempotency key.
A closing checkpoint that failed before Engram accepted it leaves the grant
open, and the same-process rebind that follows closes it with the report the
record still holds, while restart recovery, which has none, closes it bare.
A report Engram refuses (an answer, not a lost or timed-out call), whether on
the turn's own close or on a rebind's recovery checkpoint, is not resent: it
is dropped and logged, and the grant's next closing attempt goes bare, as
every checkpoint did before turns reported observations. Any refusal counts,
not only one about the payload, because a refused report resent forever
would hold the grant, and the session's queued prompts, until a restart;
dropping it costs at most that turn's evidence. A closer that finds another
checkpoint of the session already in progress skips at once, without taking
a capture it could never report.
Only a session bound to claimed work reports it: Engram admits host evidence
solely through an exact work binding, so an unbound session's checkpoint
carries no observation rather than one the evidence gate would reject.
A turn that changed source under a grant that mediates no local mutation (a
read-only child's, or one issued from a retained evaluate prepared under an
earlier effect set, as described under admission above) reports nothing
rather than an observe-only claim, and logs the omission. Restart recovery, the
compensating checkpoint of a superseded begin and the project-reset exit
checkpoint close grants whose turn this process never saw end and report no
observation.

### Content revision

Every source basis TermAl reports, at a turn's begin and close, at each
check's start and end, and as an evaluator's declared fingerprint, is taken by
one function (`src/content_revision.rs`) and reads `content-v1:<sha256>`.
Engram treats the value as an opaque string. It checks only the value's
shape, then compares it for equality.

- **What it covers.** Every path Git lists as tracked, or as untracked and
  not ignored, as it is in the worktree now. A present file counts with its
  content and its mode: the executable bit on Unix, `100644` on Windows,
  which has none. A symlink counts with its link text and mode `120000`. A
  path that is not there has no entry, as if its deletion were committed; so
  has a tracked file now replaced by a directory, whose own files are listed
  as untracked.
- **What does not move it.** HEAD takes no part, and the index none in
  content or mode. Committing already reported content, staging it,
  fast-forwarding to the same content, or `update-index --chmod` leaves the
  revision where it was. The index decides only which ignored paths are
  listed: a force-added ignored file counts, and `git rm --cached` of it
  removes it. A gate that ran
  before the commit still speaks for the committed tree (tm-21yl), and an
  evaluation of the same content stays fresh (tm-5gi4). Two worktrees that
  hold the same content have the same revision.
- **Line endings.** A file with a line-ending contract counts byte for
  byte, so rewriting its line endings is a change: one whose
  `.gitattributes` set `eol=` (as TermAl's `*.sh text eol=lf`) or unset
  `text` (`-text`, which `binary` sets), and on Unix an executable one,
  whose interpreter line a CR would break. Every other file counts with each
  CRLF read as LF exactly when Git's `core.autocrlf` would convert it on the
  way in. Git takes a file for binary, and leaves it alone, when it holds a
  lone CR or a NUL anywhere, or when its printable bytes divided by 128 are
  fewer than its non-printable ones; the tests compare the rule with Git's
  own conversion. So Git rewriting such a file with the other line ending
  (checkout, reset, stash), or a checkout made with another
  `core.autocrlf`, does not move it. The pinned runner cannot leave this to
  Git: it drops the system and global configuration where `core.autocrlf`
  lives. Git also leaves a file alone when the index's copy holds a CR; the
  revision does not consult the index for that. The revision is not byte
  identity: the known limit is a file with no contract rewritten from LF to
  CRLF, which keeps its revision.
- **What else moves it.** Any tracked file does, the tracker's own included.
  In TermAl, a `bd` write changes files under `.beads`, which are tracked, so
  it moves the revision like any other edit.
- **Platform semantics.** On Unix the filesystem's executable bit counts even
  where the repository sets `core.fileMode=false`, so a chmod alone moves the
  revision. On a filesystem that ignores case, renaming a file in case only,
  outside Git, does not move it. On Windows, a tracked file another process
  holds open without read sharing cannot be read, which fails the capture.
- **When there is none.** The capture uses the review freeze's pinned Git
  runner and budget. A repository with a submodule gets none: what is inside
  a submodule is not read, so a checkout, an edit or a deinit there would
  leave the revision equal, a false "unchanged"; the capture fails closed,
  as the freeze does (a repository such as PhoenixCodeNav, whose index holds
  gitlinks, has no basis, as before). A repository that configures Git
  filters gets none: nothing here runs a filter, and the refusal is kept
  only for uniformity with the freeze. Nor does an untracked nested
  repository, one path that is not UTF-8 or not safe (empty, absolute, with
  `..` or `.` or an empty component, with a newline, or, on Windows only,
  with `:` or `\`), a file that changes while it is read or cannot be read
  for any reason but absence, or an exceeded bound (200,000 paths, tracked
  and untracked together, 128 MiB for one file, 512 MiB read in all, the
  forty-second budget; a revision finished after the budget is not
  returned). On a filesystem that ignores case, a tracked directory renamed
  in case only fails the path check the same way, because the index still
  spells it the old way. So does a tracked `.gitattributes` deleted from the
  worktree but not yet in the index: Git would then take its attributes from
  the index, and staging the deletion alone would change which files count
  byte for byte. One such path costs the whole tree its basis. The
  report then goes without one, the conservative path described above, and
  an obligation opened without a basis can only be waived. A path that is
  simply not there, its directory gone or turned into a file, has no entry
  and fails nothing.
- **Cost.** Every capture reads every listed file, with no cache: at turn
  begin and close, at each check's start and end, and for an evaluation at
  request and at submission. The cost grows with the bytes listed. A capture
  that takes two seconds or more is logged with its entry count and bytes
  read, so the cost is measured where it runs. Past the bounds above a tree
  has no basis at all. A per-file cache keyed by size and time would cut the
  cost, at the price of its own staleness risk; there is none yet (tm-heis).
- **Stalled reads.** A file read cannot be interrupted, so every capture a
  turn or an evaluation waits for runs on its own thread and is waited for
  at most the budget. At a turn's begin and close, a capture that overruns
  counts as not taken, the conservative path described above, and the close
  keeps one budget for its basis and its checks together. The thread is left
  to finish its read; at most 32 such threads take turn bases in the host at
  once, and past that a turn's basis is not taken.
- **Not the review-freeze fingerprint.** Review freezes keep their schema-1
  fingerprint, which hashes HEAD, both diffs and the untracked files. The two
  values differ by construction, even for an uncommitted tree.
- **Switching from the earlier revision.** Before this, the source revision
  was the schema-1 fingerprint. On a run open across the upgrade, the first
  basis after it differs from every earlier one although nothing changed.
  Such a run needs one more passing test and, where it was evaluated, one
  more evaluation. Upgrade while no evaluation is pending.
- **Ignored files are not source.** A write only to a path the revision does
  not cover (an ignored file, a nested worktree under an ignored folder)
  leaves the revision where it was, so a turn whose begin and closing bases
  both exist reports no source change for it, whatever the file watcher saw.

### Source root

A root session usually starts in the project folder and does a claimed
item's work in a linked worktree. By default every basis above is taken on
the session's workdir, which would measure the main checkout while the work
happens elsewhere. The agent therefore names the item's worktree once with
`termal_name_source_root { work, path }` (tm-5gi4 phase 2, Greg's option B;
`src/engram_source_roots.rs`), and TermAl measures that tree instead.

- **Who names it, and what.** Only a root session that holds a live claim on
  `work` (its id or short reference), as `engram work core held` lists it
  under that session's own Engram session id, with Engram control on for it
  and its project (a name counts only for mediated turns). A project, folder
  or Engram store that changes while the name is being checked makes the
  call a conflict, and nothing is named. The path must be (a) the root
  of a worktree TermAl can measure, (b) of the same repository as the
  session's workdir (the common Git directory, read from the file system
  and compared exactly, since on a case-sensitive volume two repositories
  may differ only in case), (c) registered with it (the main worktree, or a
  linked one whose own directory names `<root>/.git` back; a copied or
  pruned `.git` is refused), and (d) inside the project folder on its
  canonical path, because the acceptance evaluator runs there and a
  delegated session's folder must lie inside its project. A root on a
  network share, or on a drive mapped to one, is refused, whatever the
  spelling (a verbatim alias of the network redirector, such as
  `\\?\GLOBALROOT\Device\Mup\…`, included, before anything resolves it):
  TermAl never resolves such a path while measuring, and the
  evaluator cannot start there. A path is at most 4096 characters, and a
  Windows drive-relative spelling (`C:wt`) is refused, since it would
  resolve against the host process's folder on that drive; Git Bash's
  `/c/…` spelling, as its `pwd` prints it, is read as `C:/…`. Omitting `path`
  clears the name; a blank or null `path` is refused rather than read as a
  clear, and the MCP tool sends a present `path` as written and refuses one
  that is not a string. A delegated session names nothing: it holds its own
  claim, and nobody may name a root for a claim they do not hold, so its
  claim has no named root and its turns are measured in its workdir, its
  own worktree (the tool is withheld from it; see
  [Agent delegation sessions](./agent-delegation-sessions.md)).
- **What is kept.** One entry per work and store, persisted with the host's
  metadata: the claim it was named for, the canonical root, the repository
  key, who named it and when, and a generation unique to the name host-wide:
  naming the same root again under the same claim keeps it, while another
  root, another claim or a name after a clear gets a new one, from a
  persisted counter, so no generation is given twice. It applies only to
  that claim, so a new claim on the same work does not inherit an old tree;
  the claim's fence is recorded but not matched, so a renewal keeps the
  root. At most 64 entries are kept, and a full list refuses a new name,
  naming the entries, rather than evict one.
- **When it takes effect.** At the session's next admission: the turn
  running when the name is given keeps the root it began with, and its tests
  and basis are not moved.
- **What moves to the root.** A turn on that claim takes its begin and
  closing basis there; a recognised test is credited only when it ran in
  the root (the command is still read from the session's workdir, where its
  runtime runs it), and its snapshots are taken there; the file watcher's
  hint, where it still decides (no closing basis), counts only paths inside
  the root, which may lie beside the workdir rather than inside it, and
  counts them whichever session's scope the watcher routed them to. The
  same set is the turn's "files changed" summary, so an edit in the root by
  another session working in the same tree is listed in this turn's
  summary as well as in that session's own: the watcher knows where a
  change landed, not who made it, as for any write under the workdir. The
  hint must never miss the turn's own edits in its root, and the summary
  errs the same way; the
  root is added to the session's writer worktrees for overlap while the
  turn's grant is held, and another session's check still open in it is
  marked when the turn is admitted with it, as one in the workdir is at the
  turn's start; and an acceptance evaluation requested by the claim's
  session runs its evaluator child in the root, takes its fingerprint
  there, and names the root in its notice. What stays on the workdir: how
  commands are read, the shell's `cd` tracking and the workdir's worktree
  key.
- **Exactly that path.** A named root is measured on its stored path and
  never through the workdir's walk to the nearest `.git`: a root that was
  removed, or lost its `.git`, has no basis, never the main checkout's. At
  landing, request the evaluation while the worktree exists and remove it
  after the verdict is recorded. Naming the main checkout is possible, and
  takes the shared tree on the item: another session's edit there becomes
  the item's change.
- **Renaming or clearing during a turn** seals the old root's revision for
  the session's turn running then in that root under the same claim, and
  the response reports the seal only when that turn took it. A turn of
  another claim measured in the same worktree (two works may name one) is
  not sealed: its work still names the tree, so its later edits there
  count. Only a turn already running with that root when the naming call
  began is sealed, and only while it still runs: a turn admitted during the
  call, whose start the capture may predate, and a turn already finished
  are not, and with no such turn no capture is taken. The sealed turn's
  close uses a live capture of the old root when
  it still exists, and the sealed revision only when the root is gone (its
  path missing, or no longer a worktree root), as a lookup that says so
  shows; a denied or failed lookup, like a root that exists but could not
  be measured, leaves no basis. The capture and that lookup run together
  within the close's bound. A sealed revision does not know of edits made
  after the seal.
- **Invalidation.** An evaluation by an evaluator child records the root,
  claim and generation it was requested on, or, with no root named, the
  claim the requesting session was bound to. Its first submission is
  refused, finally, when the work no longer names that root under that
  claim and generation, or when a root has since been named for the
  recorded claim on the evaluated work, whatever the requesting session is
  bound to by then, and whether or not a revision was taken at the request;
  its submit-time capture uses the stored path. The root is checked again under the lock that admits the
  first write, after that capture: naming commits under the same lock, so a
  write admitted there was for the root as it was then named. The request
  itself looks the root up again under the lock that creates the evaluator,
  after its own capture, and is refused when a rename, a clear or a first
  name landed in between.
- **When an entry ends.** On a clear by a session holding the work's claim;
  when the session that named it no longer exists (at its removal and at
  restore); and, once its claim has ended, at that session's next naming
  call, when a complete held-claims read of it no longer lists the claim.
  The admission refresh reads only the selected binding, so it ends none:
  until one of these happens an entry of an ended claim stays, though it no
  longer applies. A new name on a list that is still full once the caller's
  own such entries are gone first ends such entries for every other naming
  session that still exists, one held-claims read under each
  one's connection (up to eight at once), and only then refuses; a refusal
  keeps what the reclaim ended. A read that left claims out, listed a claim
  without its id, failed, or did not start within the naming budget ends
  nothing, and a read ends only entries that existed before it began, never
  one named meanwhile. Likewise a name is compared with the work's entry as
  it stood before the call's first read: a name that landed since, for a
  newer claim say, makes the call a conflict rather than be overwritten.
  An entry named in a store its session's project no longer uses, or of a
  project with Engram turned off, can be neither read nor cleared, so it
  stays until that session is removed; the full-list refusal names each
  entry's store.
- **How long naming takes.** One budget of 82 seconds on the server covers
  the held-claims reads, the path's validation (on its own thread), the
  reclaim and the captures (each capture counted with the turns' capture
  threads); what has not started when it runs out is skipped
  conservatively: no reclaim, no sealed revision, the new root reported
  unmeasured. The captures stop two seconds short of its end, leaving that
  for the commit, so a slow tree leaves the new root unmeasured rather than
  costing the name. A validation that outlasts it, or a name that would be kept
  after it, is refused with 503 and nothing is named. At most four path
  checks run at once on the host, an abandoned one keeping its place until
  its thread ends, so a stalled volume cannot pile up threads; a call past
  that gets 503 too. The MCP bridge waits
  that budget on top of its normal request timeout, so a tool call that
  reports a failure never leaves a name the server kept. The budget starts
  when the handler runs: the time the request waits for a blocking worker,
  and the final write of the state, fall within that normal timeout. A Codex
  caller waits longer still: TermAl gives Codex the delegation server's
  `tool_timeout_sec`, covering the longest bridge allowance
  ([Agent delegation sessions](./agent-delegation-sessions.md#completed-codex-child-thread-lifecycle)).
- **What the agent and the operator see.**
  - *The checkpoint card* of a bound turn names where it was measured: the
    root and the item, or "session workdir (no worktree named)". The card
    is built from the root the turn was admitted with, so a bound turn
    whose begin-time capture never ran shows the workdir even if an entry
    exists.
  - *When the agent gets a host line:* at a newly bound claim, after it
    names or clears a root, and once a recognised test of a mediated turn
    gets no check, whether or not it finishes, with the remedy. That is when
    the test started in another worktree than the one the turn is measured
    in, and when TermAl cannot confirm it did not: because it ran after
    another command on its line, because the runtime reports no directory
    and its shell may be back in the session's workdir, or because TermAl
    cannot place the shell (a change it cannot follow, or another command
    still changing it).
  - *The form up front.* The named-root line names the one-call form,
    `pushd "DIR" && TEST` with DIR the directory in the root where the test
    runs and nothing piped, redirected or chained after the test, to a
    Claude session, whose runtime alone reports no directory and runs the
    line as the agent wrote it (Codex reports its directory and wraps each
    line in a shell of its own; an ACP runtime may do either), when the
    root is one the form reads. TermAl records a Claude command only from
    its `Bash` tool, which runs bash, so the form never meets Windows
    PowerShell 5.1, which cannot read `&&`. A test Claude runs through its
    `PowerShell` tool is not recorded at all, so it gets no check and no
    host line, in whatever shape. Any other session is told the
    form only once a test of its gets no check in a shape the form carries.
    On Windows, wherever the form is named, DIR is said to be written as a
    Windows path (`C:\…` or `C:/…`, not Git Bash's `/c/…`), the spelling
    Claude's Bash tool favours but the form does not take.
  - *The form in a remedy.* A cannot-confirm line, and a started-elsewhere
    line of a root session with a named root, name the form only for a
    simple test the agent ran as a bare line (never for a line its runtime
    wrapped, which could not run it), and only where the form can read the
    worktree's path. They leave DIR to the agent: TermAl names no directory
    it would have to guess, since a test meant for one inside the worktree
    would run somewhere else. They name a test's own command up to 256
    bytes, and `<test>` past that.
  - *A cannot-confirm line's remedy* follows from four facts, so each is
    one the session can carry out:
    - where the form can carry the test, the form: it counts wherever the
      shell is;
    - else, for a runtime that reports its directory, the test as a line
      of its own with its working directory set where it should run (a `cd`
      in a call of its own would not carry over there);
    - else, for a session whose workdir is in the worktree, `cd "DIR"` in a
      call of its own, DIR the absolute directory in the worktree where the
      test runs, which TermAl follows, then the test as a line of its own:
      whether TermAl placed the shell, lost it, or saw the test after
      another command, this keeps the directory the test was meant for;
      where TermAl follows no `cd` into the worktree's path (one a shell
      would expand), a new session instead;
    - else a session whose workdir is in the worktree.
  - *Near misses.* A test run after another command on its line may have
    been meant for another project, so its remedy is offered only "if it
    was meant to count here". A bare line that starts as the one-call form
    but holds more than its one test alone (piped, redirected, chained,
    wrapped, or with a command between `pushd` and the test) is told so,
    with the form as its remedy where the form can name the worktree,
    naming `<test>` where TermAl cannot rebuild the test alone from the
    line. So is a bare line on Windows that starts as the one-call form but
    writes its DIR in Git Bash's spelling of a drive (`/c/…`): it is told
    that, and, where its test is not alone either, that too, with the form
    and DIR written as a Windows path as its remedy.
  - *Started elsewhere.* A started-elsewhere line without a named root
    tells the agent to name that worktree. A delegated session, which
    names no root, gets no bind line, and its withheld-test line tells it
    to run the tests in its workdir.
  - *Delivery.* A new line about a test that got no check replaces an
    earlier one not yet delivered, so a run of them never pushes out the
    bind or name line. Lines not yet delivered accumulate, at most four, so
    a later one does not hide an earlier one; a line given again moves to
    the end instead of repeating, so the last line tells the latest state
    (name, clear, name again ends on the name). They come after the Engram
    context fence, when one is delivered, and before the prompt. A line
    leaves once the runtime accepts a prompt that carried it, turn grant or
    not, so a dispatch refused before that keeps it for the next; a line
    set after the prompt was built (a rebind while the begin is recovered)
    was not in it and reaches the next prompt, and so does a line the
    prompt carried that was given again after it was built, so the next
    prompt still ends on the latest state.
- **Limits.** The show receipt of the installed Engram carries no work id,
  so an evaluation is matched to its root by short reference. An evaluation
  is measured in a named root only when the requesting session is bound to
  the claim that named it; a session holding two claims that asks for the
  evaluation of the one it is not bound to is measured in its workdir. A
  same-session evaluation takes its fingerprint in the root but records
  none, having no evaluator child: a rename or a clear after it does not
  refuse its submission, and the tracker's own freshness check is what
  stands. A
  change made between two mediated turns (the naming turn's edits, a
  runtime-started turn) is not reported by either; option A', a separate
  Engram record, is planned for it. The main checkout of a
  `--separate-git-dir` repository cannot be named, since its `.git` file
  names no linked worktree; a linked worktree of it can.

### Test evidence

A test the agent runs during a mediated turn on claimed work is reported on
the same closing checkpoint as typed verification evidence with its
environment, so Engram's stock source-change rule and a criterion bound with
`--bind N=test` can be satisfied by what the host saw, not by what the agent
says. TermAl does not run checks itself; it observes the commands the runtime
reports.

- **Which commands.** One shell wrapper is removed (`bash -lc 'X'`,
  `pwsh -Command X`, `cmd /c X`), and the first command of the line, with
  its first words, must name a test runner: `cargo test`,
  `cargo nextest run`, `npm test`, `npm run test`, `pnpm test`, `yarn test`,
  `npx vitest`, `npx jest`, `pytest`, `python -m pytest`, `go test`, or the
  test launcher (`node …/test-launcher.mjs full|live`, or `focused -- X`
  where X is itself a recognised test, since a focused run executes whatever
  follows `--`), with `--detach` only for a full gate, whose launch is
  carried (below). A run with `--no-run`, `--list`,
  `--collect-only` (pytest's `--co`), go's `-list` or `-c` tests nothing
  and is not recognised. A line may instead be the one-call form, exactly
  `pushd "DIR" && TEST`: a bare line with no wrapper, `pushd` in lower case
  (bash's builtins are case-sensitive), one double-quoted native absolute
  DIR made only of ASCII letters, digits, spaces and `_ . - /`, with `:`
  only after a Windows drive letter and `\` on Windows alone (no `.` or `..`
  step, no doubled or trailing backslash, no network path, and on Windows
  no Git Bash `/c/…` spelling, which PowerShell reads as `\c\…` on the
  current drive), then `&&` and
  one simple
  recognised test on its own, with nothing piped, redirected or chained
  after it (such a test could not pass anyway: a pipe hides its exit
  status). Bash, PowerShell 7 and cmd all run this line
  as a change of their own directory, across drives too, and run the test
  only once it succeeded, so the test runs in DIR; its check fingerprint is
  the test's own. A non-zero exit may come from the change or the shell's
  reading of the line (Windows PowerShell cannot parse `&&`) before the
  test ran, so it is a failure only when the test's runner stated its
  result in the output (its result line, as the summary shows it: cargo's
  `test result:`, pytest's closing summary, the vitest and jest totals,
  go's package lines, the test launcher's verdict), since the runner then
  ran and the line exits as its test did; otherwise it is an unknown
  outcome (Engram's `indeterminate`), which still refuses completion, and
  the form never yields failed evidence that is not the test's. Its
  evidence says which: the summary names the whole line (its test as
  normalised, a directory too long for the bound shortened from its start)
  and, for a non-zero exit not known to be the test's, that the exit does
  not say whether its `pushd` or its test failed, and the refs give no exit
  then. Anything else
  that runs a test after another command, `cd DIR && TEST`, a wrapper
  around the form, the form with something after its test, `pushd DIR;
  TEST` or another directory change included, is not reported; the agent
  is told why, with a remedy (a best effort that reads only unquoted `&&`
  and `;`). Only a command line the runtime
  reports is recognised: an
  ACP call that gives only a title (`Run tests`), or a Claude call shown by
  its description, names no test, and a script of several lines is not one
  test. A PowerShell or cmd script given as several words is taken as
  written, its quoting kept, so each argument is the one the runner saw.
- **Which worktree.** A check is credited to the turn's worktree, the
  claim's named [source root](#source-root) when there is one and else the
  session's worktree, only when TermAl can tell it tested that worktree:
  the command runs there (in
  the directory the runtime reports, Codex's item `cwd` or ACP's
  `rawInput.cwd`, else the session's workdir, a subdirectory included), and
  no argument names a place outside it. Every argument, what follows each
  `=` in it (`--flag=value`, and a setting's own value in
  `pytest --override-ini=testpaths=../other/tests`), every value attached
  to a one-letter option (`pytest -c../other/pytest.ini`), and each item of
  any of them read as a list (split on whitespace or `;`, as pytest splits
  `testpaths`) is resolved as a path from that
  directory; one that leads out of the worktree, as an absolute path, a `..`
  climb or a symbolic link or junction, names what the command tested (so
  `cargo test --manifest-path ../other/Cargo.toml` is not a check), and so
  may one starting at `~` or holding a variable anywhere
  (`tests/${SUITE}`, `%SUITE%`, cmd's delayed `!SUITE!`, `$env:SUITE`),
  whose value TermAl cannot see, so neither is a check either. A PowerShell
  wrapper that starts its script elsewhere (`-WorkingDirectory`, `-wd`) is
  not read as run in place, so its test is not a check, and its command may
  write in any worktree. An argument is judged as the shell
  running the line passes it on: bash drops its backslash escapes
  (`lin\ked` is `linked`), PowerShell and cmd keep them (a Windows path,
  and a quoted UNC `"\\server\share"` keeps its prefix; only a run of them
  meeting a quote folds, as a Windows program splits its command line),
  and a line no wrapper names is read both ways. It is also judged piece by
  piece, as a PowerShell array (`a,b`) passes it, and a line holding
  syntax a shell evaluates rather than passing on as written (an unquoted
  `(…)`, `{…}`, backtick, `^`, or `@` starting a word, as in
  `pytest ('../x')` under PowerShell or `pytest {a,../x}` under bash) is
  not a check. A network path (`\\server\share`, `//server/share`) is
  never resolved, since that can block for a network timeout: as an
  argument, a reported directory or the session's own workdir it leads out,
  and a `cd` to one loses the shell. On Windows, Git Bash's `cd /c/…` is
  `C:/…`. A path is resolved through the links
  on its longest existing part, so a test selector or glob under a link
  (`linked/test_x.py::test`, `linked/test_*.py`) leads where the link does,
  a node selector's file (`test_alias.py` in `test_alias.py::test_ok`) is
  resolved on its own, since it may itself be a link, and a directory
  spelled through an alias (`/var` for `/private/var`, a Windows short name)
  is the directory it names. These paths are compared as the file system
  stores them, case included, so two worktrees that differ only in case on
  a case-sensitive volume stay two (overlap, where merging two places only
  makes a check unknown, ignores case on Windows and macOS). A glob is matched by the runner, not by TermAl:
  a file it matches through a link inside the worktree, like a test a runner
  discovers there, is outside what the revision covers. A runtime that
  starts one command more than once (ACP: the call, then pending or in
  progress updates, each carrying only what changed) keeps the last
  directory it reported for that call, a start that gives no command line
  leaves the command as another start named it, one that gives a line names
  what runs now, test or not, and a check stands only while the starts name
  the same test in the same place: one that moves it elsewhere, even within
  the worktree, drops it, and the call starts no check again until it ends,
  since a new one would begin its record after writes the old one saw. A
  test a call names only after it started (a title, then an update) began
  before its first snapshot, so its outcome is unknown. An
  ACP update without a status, and the one that ends the call, count the
  same way when they say what the call runs or where, but never start a
  check, whose first snapshot must come before its test runs. The
  launcher's `--engram-binary`
  value, the pinned binary a live run uses, may lie anywhere. The worktree's
  revision covers a link inside it, not what the link points to. The
  resolution runs off the state lock. Claude reports no directory, and its
  shell keeps a `cd` between calls (unless it is set to return to its
  project), so a command it runs is judged both from the workdir and from
  where the calls before it presumably left that shell, and is a check only
  when it tests this worktree from each. So a test run after a `cd` made in
  an earlier call is no check of a named root that the session's workdir
  lies outside; the one-call form `pushd "DIR" && TEST` is judged from DIR
  alone, wherever the shell was, and is how such a session's test counts. A call
  that starts with `cd DIR`
  (`Set-Location`, `pushd`) for a literal DIR that exists, bash reading it as
  written, and changes directory nowhere else, moves the presumed place
  there once the call has succeeded: a denied call moves nothing, and one
  that failed or ended unreported may have stopped before or after its
  `cd`, which loses the shell, and so does a DIR whose `..` a shell takes
  against the path as written (bash's logical `cd`) but the file system
  takes after following a link (`/other/link/..`), since TermAl cannot say
  which place the shell is in. A quoted script handed to another shell
  (`bash -lc '…'`) moves nothing; any other directory change (no, a
  computed or a network target, `popd`, one after another command, behind
  a keyword or a loop (`then cd`, `do cd`), in a group, subshell, pipeline
  or background job, `eval`, two at once), or any word that could name one
  (`xargs cd`, even `echo cd`), loses the shell, and no check is kept until
  a `cd` to an absolute path places it again or a new runtime starts its
  shell afresh. A change made by a script a call runs or sources, or by a
  shell function or alias, is not seen. The same holds for any command
  whose runtime gives no directory, whichever report of an ACP call names
  its command line. A runner that finds what it
  tests by name (a Go import path, an installed Python package) is taken to
  test the worktree it runs in.
- **Check fingerprint.** The producer observation's action fingerprint, which
  a `--bind N=test:FINGERPRINT` pin must equal, is the lowercase hex SHA-256
  of the UTF-8 bytes of the normalised command line: the wrapper removed,
  whitespace runs outside quotes collapsed to one space, trimmed. For
  example
  `cargo test --test source_file_size -- --nocapture` hashes exactly that
  string.
- **Outcome.** Only a simple command's own exit status can succeed or fail:
  a line with a pipe, a list, a redirection, a backtick or a `$(`
  substitution is reported as unknown, since `cargo test | tail` exits 0
  when tests fail. Codex and ACP report the exit code (0 succeeded,
  otherwise failed; none, unknown). Claude reports none: a Bash result
  without `is_error` succeeded, one whose text starts `Exit code N` failed,
  and any other error, or an interrupted command, is unknown. A background
  run's result marks its launch, so it is not reported, and it counts as
  running for the rest of the turn, taking no closing snapshot; a denied
  command stops counting as running. A launcher full gate launched in the
  background, or detached with `--detach`, is instead carried past its
  turn and settled when its run ends (Carried background gates, below).
- **Passed needs evidence.** A run that tested nothing can exit 0, so a check
  claims success only when a result line shows tests passed: cargo's
  `test result:` or nextest's `Summary` with more than zero passed, pytest's
  closing banner or `-q` summary (`3 passed in 0.10s`) with passed tests, a
  vitest or jest `Tests` total with
  passed tests, a go package `ok` without `[no tests to run]` or
  `[no test files]`, or a passed test stage of the launcher mode that ran:
  a full gate's `rust-tests` or `vitest`, a live run's `engram-live`. A
  focused launcher run never shows it, since its
  verdict does not say how many tests the wrapped command ran. Without the
  evidence the check is unknown, never passed. What a check attests is that
  this command line ran in this worktree and printed passing results, not
  that the tests it ran are genuine: the worktree decides what `npm test`
  runs (its revision covers that), and the agent's shell decides which
  program a name finds.
- **Overlap.** A check is open to writes from its start until both its
  snapshots are taken. It is unknown when, while it is open, another command
  or a file edit from the same agent is reported, or another writable
  session in the same worktree (any subdirectory of it, however its path is
  spelled) starts a turn or reports a command or an edit; or when such a
  session is in a turn as the check starts or ends. A session's command
  counts in the worktree it runs in, not only in the session's own: the
  directory its runtime reports, or, for a runtime that reports none, the
  workdir and where its shell is presumed to be, and where a `cd` of its
  own leads, one in a script it hands to a shell wrapper included. Where
  TermAl cannot follow that shell (a `cd` after another command, in a
  pipeline or group, or more than one on a line), the shell is lost among
  the places it may be: where it was, where a move still pending leads, and
  each absolute literal target of the changes since, at most 32 places. A
  line that starts with a `cd` to a literal absolute directory and changes
  directory nowhere else, outside a pipeline or group, follows the shell
  again once it succeeds, unless another command's move is still pending as
  it is reported. Any other change cannot be placed, now and for the lost
  shell's later commands (what that means follows this list): one whose
  target TermAl cannot read
  (`cd $X`, `cd -`, `popd`, PowerShell's `cd..` and `cd\`, a drive switch
  such as `D:`), a relative one (a subshell may leave the shell elsewhere to
  take it from, and a link may make its `..` land elsewhere), a path that
  does not exist yet when TermAl reads the line (the line may make it
  first), even for a lone `cd`, one given more than its target (cmd's
  `cd /d X`), and every change on a line that has a brace outside quotes (a
  function body or script block, which may repeat it, or a brace expansion
  such as `cd {a,b}`), repeats it in a loop, runs a command or process
  substitution (`$(…)` or a backtick, even inside double quotes, or
  `<(…)`), has a comment or a backslash next to a quote (whose quoting
  TermAl may read otherwise than the shell), defines an alias, a function
  or a trap, runs another shell, sources a script or runs one in the shell
  itself (a `.ps1` or `.bat` script, `call`, `Invoke-Expression`), or runs
  a command known to make, move or remove paths (`ln`, `mv`, `rm`, `rd`,
  `find -delete`, any git subcommand but those that only read, behind any
  keyword or wrapper such as `then` or `sudo`), and on a line where
  anything but a command that only reads (`ls`, `cat`, `echo`, a read-only
  git subcommand and the like) runs before its last change, since any other
  program may re-point a link the target passes through. After the last
  change, the list of programs that change paths is best effort, as for a
  shell TermAl follows: another program that removes a nested `.git` there
  is not seen, nor is a change of location a function defined earlier
  makes, nor a script PowerShell or cmd runs in the shell itself without a
  `.ps1` or `.bat` word (`& $script`, `.\setup`, a batch file run by its
  bare name). A relative `cd` of a lost shell's command cannot be placed
  either, for that command as it starts and for the shell's later commands;
  nor can the shell's later commands after a relative `cd` TermAl resolved
  from a place the shell has since left (another command's move became
  pending in between), nor a PowerShell wrapper told to start elsewhere. A
  command TermAl cannot place counts where its session works: its workdir's
  worktree and the named source root its turn works in, beside the places
  it is known to be among. It does not count in every worktree. Read that
  way, one session with a command running from a lost shell (a Claude
  background call counts as running for the rest of its turn) marked every
  check on the host, and no check earned credit while several sessions
  worked. What this leaves unseen: a session that went into another
  session's worktree by a `cd` TermAl could not follow, and wrote there
  only transiently or only in a directory Git ignores. A write that stays
  changes the source snapshots of the check it landed under, and for a
  carried gate the launcher's fingerprint and the workspace watcher's view
  too. A session whose own workdir was never resolved still counts in every
  worktree, since nothing says where it works. A command of a Claude
  session that only reads (what the read-only reviewer policy allows, read
  by the Bash rules its commands follow) writes nowhere and counts nowhere;
  another runtime's command always counts. A session in a turn with
  such a command running elsewhere counts there until the command ends; its
  next turn forgets a background command. A command that writes elsewhere
  by path (`git -C`, a redirection) is not seen, nor is a directory run as a
  command, which a shell with `autocd` set changes to. Each is marked on
  the check as it happens, so a later report from that session cannot erase
  it, and a mark made while the closing checkpoint waits for the snapshots
  still counts. A read-only delegation child does not count. A command that
  reuses an earlier command's key is a new command. Which worktree a
  session works in, and each of its commands runs in, is resolved off the
  state lock as it starts a turn and whenever it reports a command or an
  edit, and kept on the session, and marking under the lock uses what was
  resolved last, never the file system or a cache other sessions share; a
  session whose worktree was never resolved for its current workdir (its
  workdir changed since) counts as the same worktree,
  and a network path is keyed as written, unresolved. Writes through TermAl
  itself count the same way: a file saved
  from the editor, a review document saved under `.termal/reviews`, a Git
  file action, sync or commit (whose hooks may rewrite files) and a
  terminal command, each as it starts and as it ends. A
  command abandoned before it ran (a denied Claude call, a declined Codex
  command) drops its check, a turn
  keeps at most as many checks as it can report (a new one drops the oldest
  finished one; while every kept check still runs, a new test is not
  checked and takes no snapshots), and the closing checkpoint drops the
  turn's checks once its
  report holds them. A grant that ends without its report (a compensating
  close, a project reset) leaves its checks until the next grant clears
  them; they are never reported, so they no longer count as open.
  What TermAl cannot see: a process an agent left running in the background
  after its own turn, a terminal command already running when a check
  starts that ends after the check settles, and writes from outside TermAl.
- **Freshness.** The content revision is taken when the check starts
  and again when it ends, each on its own thread. A check whose two
  revisions differ, or either is missing, is not reported: the source
  moved while it ran. Equal fingerprints bound the window between the two
  snapshots, not an edit that landed between the command's start and the
  first snapshot, which is why overlapping activity makes the outcome
  unknown. A snapshot covers the whole worktree the workdir lies in, so a
  session in a folder under a repository at the home directory (a dotfiles
  repository) snapshots that whole repository at each check, within the
  freeze's budget (tm-x9fz tracks bounding it).
- **Order.** Engram requires the evidence at or after the change it answers,
  in time and in the feed. For each reported check, in start order: when its
  revision differs from the last one reported (the turn's begin-time basis
  at first), a change observation at that revision timed at the check's
  start (only under a grant that mediates local mutation); the producer
  observation (`observe`, timed at completion); environment evidence, shared
  by checks on one revision and toolchain with no change reported between
  them, so a check never cites an environment observed before the change it
  answers; and the verification evidence
  citing both. The turn's own observation follows; under a grant that
  mediates local mutation it reports a change only if the source moved again
  after the last check, judged by the revisions alone: TermAl's file-change
  tracking, which can arrive late and would reopen a change a check
  answered, no longer adds to it, so an edit only the tracking sees (an
  ignored path) after the last check is not reported as a change. Under a
  grant that does not mediate local mutation it is judged against the
  begin-time basis, as without checks, and withheld when the source changed.
  Observation ids include
  the check's place in the turn, so they stay unique when a runtime reuses
  a key.
- **Environment.** TermAl runs the version commands itself, with its own
  rights and outside any sandbox the check ran in, so it never runs a
  program the workspace could have written or picked. `toolchain` is named
  only for `cargo` run by name, which is the first `cargo` on TermAl's
  `PATH`. When that is rustup's own proxy (rustup lies beside it), the
  toolchain is its `+toolchain` selector, when that is a plain toolchain
  name, or else the toolchain `rustup show active-toolchain` names in the
  directory the check ran in (rustup reads the workspace's toolchain files
  and overrides without running them). `rustup which` then
  locates that
  toolchain's `rustc` and `cargo`, which must lie outside the worktree, and
  the label is the first line of their `-V`, each run by its path from its
  own directory, with rustup's automatic installs off. Only a rustup of
  1.28 or later is asked (`rustup --version` first): an older one ignores
  that setting and may install the toolchain a workspace file names while it
  is only asked to show it, so its checks go without a toolchain. Every version command
  runs through the bounded reader the freeze's Git reads use, owning its
  whole process tree (a Windows job without a console window, a Unix process
  group). A cargo that is not rustup's proxy (a standalone install ahead of
  rustup on the `PATH`, or no rustup) is labelled with the `rustc` beside
  it, both outside the worktree, and a `+` selector names nothing for it. A
  toolchain file that names a directory, a cargo run
  by a path, and every Python, Go, Node and launcher run go without a
  toolchain: those are commonly started through version-manager shims that
  pick the program from files in the workspace, and naming it would mean
  running what those files point at, or guessing. The label is TermAl's
  resolution of the same name, so an agent shell with a different `PATH`
  could have run another cargo. The label is taken as the check starts, on
  its own thread, so a toolchain file edited after the check cannot relabel
  it; each command takes at most five seconds, and the closing checkpoint
  waits for the label within its freeze budget. When a command fails or the
  budget runs out, the check has no environment evidence rather than a
  guessed one. `sandbox` is the Codex sandbox mode
  the turn ran under, and absent for other runtimes, whose approval modes are
  no sandbox; `workspace_id` is the basis root; `capability_map_revision` is
  the bind value. Each label is trimmed, non-empty and at most 256 bytes, so
  a longer workspace root leaves the check without environment evidence. The
  fingerprint is the SHA-256 of the components' RFC 8785 JSON, which Engram
  recomputes.
- **Text.** The summary names the command and how it ended, then only the
  runner's result lines (cargo's `test result:` and nextest's `Summary`,
  pytest's closing banner or `-q` summary, the vitest and jest totals, go's
  package lines, the launcher's verdict and stage statuses without their log
  paths), with
  terminal codes stripped, within Engram's 4096 bytes. For cargo the lines
  of a file-size inventory, `PATH: N physical lines (limit M)` at the start
  of a line, count as result lines too, all or nothing: when they do not all
  fit, every one is left out and `inventory omitted: N lines over the
  summary budget` says so, since a partial list would misstate which paths
  the check covered. Raw output is left out: it can carry secrets and paths,
  and a summary Engram's redactor refuses drops the whole report. The
  references are `command:<normalised line>`, left out when longer than
  1024 bytes, and, when known, `exit:<code>`. The command line itself is
  sent as the agent ran it, in the summary's first line and that reference:
  an argument carrying a secret (`--token=…`) goes with it, and a report
  Engram's redactor refuses falls back to the turn's own observation, as
  below.
- **Bounds.** At most 16 checks per turn, the latest kept, and 4 environment
  records; a check past them is reported without one, which an unpinned
  requirement allows. A source basis over 512 bytes is not sent.
- **A success the host cannot judge is withheld.** A check whose own command
  ended successfully but which could only be reported as unknown, because it
  overlapped a write or its output shows no passing test, is not sent at all.
  Engram judges a criterion bound to a kind by the newest verification of
  that kind and counts an indeterminate one as not passed, so an unknown
  record would hide an earlier pass at the same revision while saying
  nothing the host can judge. The host logs it, and its holder is told
  before the next prompt that the check earned no credit and why. A check
  whose command failed is sent as before, overlapped or not: withheld, it
  would leave an older pass the newest record. A pass still counts only at
  the root's newest revision, so withholding cannot keep a stale one alive.

#### Carried background gates

A full gate outlasts a Claude tool call, which is capped at 10 minutes, so it
runs in the runtime's background mode or detached, and its result marks only
its launch (`src/engram_carried_checks.rs`).

- **Carrying.** When the launch of `node …/test-launcher.mjs full` (in the
  background, or with `--detach` and a successful launch) is recognised in a
  turn on claimed work measured in its named source root, the check moves off
  the turn and is carried by its session, with its start source basis, its
  claim and the root's generation. A launch in a turn with no named root, in
  another worktree, or beside another command or writer is not carried, and
  the holder is told why. A launch that can earn no credit is dropped, never
  recorded as a test result, and its holder is told why: one on a line that
  runs more than the gate (`… full; git checkout …`,
  `… full --detach && …`), since what the rest of the line wrote in that
  call is outside the fence, and a `--detach` launch that failed, which
  started no run. A launch carried less than two minutes after another
  launch of the session in the same worktree that has no run matched to it
  (one fenced before a poll found its run, one that left unmatched, or one
  dropped or not carried at its launch, which may still have started a run)
  is refused at the next checkpoint: a run found then could be either's, and
  the host does not guess. Two minutes bounds what matching can confuse,
  since it takes only runs that started at most ten seconds before a launch;
  every line that asks the holder to launch again names that wait. A
  delegated session, measured in its workdir with no named root, never has a
  gate carried and is told to leave the gate to the root session holding the
  claim. A file change the workspace
  watcher sees in the worktree while a full gate is being launched fences it
  once it is carried. A session carries at most four; a fifth drops the
  oldest, whose holder is told.
- **The fence.** From its launch until the host reads its run as ended,
  whatever the host sees that may have written in its worktree refuses it,
  and the refusal names what that was: a command another writable session
  runs where it may write there, unless that session is a Claude session
  and the command only reads, as for the holder below (a command the host
  cannot place counts where its session works, as for an ordinary check's
  overlap above, and the line then says it could not be placed and may have
  run there); a command of another session whose
  start was never reported, and a file edit another session reports, each
  where that session works (its workdir's worktree, resolved before its turn
  starts, and its claim's named source root); a write through TermAl;
  a command of the holder's own whose place may be that worktree, including
  one a later description places there, unless the holder is a Claude
  session and the command only reads (what the read-only reviewer policy
  allows, such as `git status` or `git diff`, or the repository's own
  `node scripts/test-launcher.mjs summary RUN` run from its root; the line is
  read by the Bash rules Claude's commands follow, so another runtime's
  commands always fence), named by its program (after any leading
  `NAME=value` assignments) and a fingerprint of its line, never the line,
  which can carry a secret; any file edit the holder
  reports, since an edit report names no path; and a file the workspace
  watcher sees change in the worktree (outside the directories it ignores,
  such as `.git`, `target` and `node_modules`, and outside the worktree's
  own `.tmp/`, the scratch directory the instructions send all scratch to;
  in a repository that ignores it and tracks nothing under it, as this one
  does, neither the launcher's fingerprint nor the source basis reads it,
  and the host assumes that rather than checking it, as it does for the
  watcher's own ignored names; it is matched exactly, never case-folded, so
  a `.TMP` on a case-sensitive volume still fences; a worktree nested under
  another checkout's `.tmp/` is judged against its own root), or any file change at all in a batch that touches
  more than 256 directories. The watcher reports
  late, so its changes fence even after the run was read as ended; nothing
  else does, since a write after the run's end cannot reach what it tested,
  and one still there at settlement changes the source basis. The holder's
  commands in other worktrees do not fence it, and a Claude holder's
  background launch does not move its shell into the gate's worktree: the
  Bash tool runs a background call in a shell of its own and says so in the
  call's result, so the launch's `pushd` ends with it and a later command
  with no `pushd` of its own is placed where the session's shell already was.
  Another runtime's unfinished command says no such thing, so the host loses
  that shell between where it was and where the command's `cd` leads.
  Another session being in a turn fences nothing by itself: a carried gate
  stays open for the length of a full gate, so only an act that may have
  written counts, and what that session then runs or edits is reported as it
  happens. That holds once the gate is carried. At the launch presence still
  counts: a launch made while another writable session is in a turn in that
  worktree, or while another command of the holder is running, is not
  carried (above), which is what covers a command that session started
  before the launch and whose end says nothing. An ordinary check of a turn,
  open for one command, keeps the wider rule (a writable session in a turn
  in its worktree makes its outcome unknown). What the fence does not see:
  a command run through Claude's PowerShell tool, which is not reported, and
  a write by a tool call that is neither a command nor an edit (an MCP
  tool, say). For those only the watcher is left, and it misses a write in a
  directory it ignores such as `target`, one under the worktree's own
  `.tmp/`, one in a worktree outside every watched root, and any event lost
  to a watcher error, which is logged and fences nothing.
- **Cargo's build directory.** Cargo makes a missing `target` through a
  sibling at the worktree's root named `target` plus random characters,
  which it tags and renames into place. The watcher ignores `target`, not
  that sibling, so the first build in a fresh worktree would fence its own
  gate. The host makes no exception for such a name: once the entry is gone
  nothing tells Cargo's placeholder from a source file that was written and
  removed. The launcher removes the cause instead: before its first stage
  it makes `target` itself, tagged as a cache directory, when Git ignores
  `target` at the root (so the frozen input cannot change) and neither
  `CARGO_TARGET_DIR` nor `CARGO_BUILD_TARGET_DIR` points elsewhere
  (`ensureCargoTargetDirectory` in `scripts/test-launcher.mjs`).
- **The run.** The host finds the run in the worktree's Git `review-runs`
  directory: the earliest full-gate run whose request names that root (both
  resolved, so a launch through a link or alias of the directory still finds
  its run), that started at or after the launch, less ten seconds for the
  lag between the launcher's start and the host's stamp of it, and, when the
  request names its owner (the launching session's `TERMAL_SESSION_ID`), is
  owned by the holder. A run that started in those ten seconds but had
  already ended by the launch belongs to an earlier launch, and so does a
  run a settled or dropped launch of the session used (the newest sixteen
  are kept), so neither is taken. On the run index's tick, and before each
  checkpoint, it reads the run's `results.json`; the first read that finds
  it terminal (it has `ended`, or a state other than `running`) keeps the
  record's SHA-256. The reads run off the state lock, one poll at a time, so
  a read is stored before any other poll, or a checkpoint that polls first,
  can read the same run, and a check read as terminal is not read by a poll
  again; settlement reads the record once more and refuses one that differs
  from that first terminal read. (Should two terminal reads ever disagree,
  the check is refused too.) A checkpoint of a session with nothing to read
  does not wait on a poll. A request root on a network path is compared as
  written, never resolved, since resolving it can block. A check
  that can no longer be credited (fenced, its launcher gone, its run never
  found, its six hours over) is not read again and finds no new run, so a
  later launch of the session can take one. A run directory whose request
  can never match the launch (another root, mode or owner, or an earlier
  start) is not read again. If no run is found within ten minutes of the
  launch, the launch started none. A run with no terminal record whose
  responsible process (`results.json`'s pid, else the request's
  `creatorPid`) is provably gone, by the run index's own liveness test, will
  never end. The record is read again after that test, since the launcher
  may have ended the run and exited in between, and a read that names
  another process (a detached run's creator handing over to its worker) has
  that one tested too.
- **Settlement.** At the holder's next checkpoint on the same claim id,
  measured in the same root generation, a carried check whose run is
  terminal settles. It is credited as a test check when: the record still
  has the digest first read; every stage its request lists passed with code
  0 and no error, among them a test stage the host recognises (the full
  gate's `rust-tests` or `vitest`, as for a foreground gate), and the exit
  code is 0 (a complete record that says failed is recorded as failed);
  its expected, before and after input fingerprints, and the request's, are
  present and equal; a fresh source basis equals the one taken at the
  launch; nothing fenced it; and the host still records the same generation
  for its claim. A source snapshot not ready within the checkpoint's budget
  judges nothing: the check stays carried, still fenced and pinned to its
  first terminal read, for the next checkpoint. One that failed, at the
  launch or at settlement, never will give a basis, so the check is refused
  at once. Its producer observation is
  timed at the run's `ended`, which is also the verification's
  `completed_at`; it is stored under the settling turn's grant, and the
  grant it launched under is kept as the `launched-under-grant:ID`
  reference. Its observation id is built from the grant it launched under,
  where its command's sequence and key are unique. The summary names the
  run, each stage in the launcher's own line (`rust-tests: failed exit=101`),
  the run directory and the input fingerprint; the stage lines make a failed
  run's exit its stages' own even in the one-call form, where an exit alone
  could be its `pushd`'s.
- **Refusal.** A carried check that fails any of those is dropped, not sent,
  and its holder is told before the next prompt that it earned no credit and
  why. A run that ended neither passed nor failed (stopped, or interrupted,
  such as one the launcher's `recover` settled as failed and interrupted) is
  neither a pass nor a failure: it is dropped, never recorded as failed. So
  is a run whose launcher is gone without a terminal record; the holder is
  told to settle it with `recover` and run the gate again. So is a run whose
  record lacks an input fingerprint from before or after its stages (one
  that ended before the launcher measured its input), a launch whose run was
  not found within ten minutes, one whose claim was released or whose root
  was renamed or cleared, and one six hours after its launch whose run has
  not ended, or has ended but was never settled because the holder's
  checkpoints since were on another claim or source root; the holder is told
  which. Every line about a check's credit is logged as it is set. Those
  that wait for the same prompt are merged into one, so they seldom push out
  a line about the session's source root; past 1,600 bytes the merge keeps
  the newest and cuts the front, saying so. A line a prompt in flight
  carries is not merged into, so accepting that prompt takes it out and the
  new one waits for the next. A reset of the session's Engram
  state drops its carried gates: when the project's Engram settings change,
  each holder is told its gate earned no credit, and when the project is
  removed, which ends Engram for the session, the gates are logged.
- **Restart.** Carried checks live in memory; a marker of each is persisted
  with the session. A host restart loses the checks, since the host could not
  watch the worktree while it was down, and loading the marker tells the
  holder that the run lost its credit.
- **What it attests.** The host observed the launch, observed no write it
  can see reach the worktree until settlement, and read a terminal record
  that agrees with itself and with the source. It does not attest that no
  write happened outside the host's sight: the launcher's records are
  agent-writable, and its run directory lies in the main checkout's Git
  metadata, outside the fenced worktree, so a rewrite there before the
  first terminal read is not seen. Only a Claude session's Bash tool
  reports its commands to the host: a command run through its PowerShell
  tool never reaches the fence, nor does a write by a tool call that is
  neither a command nor an edit, and only the workspace watcher, which skips
  its ignored directories, the worktree's own `.tmp/` and unwatched
  worktrees, can see what either wrote. As
  for a foreground check, the evidence model assumes an agent acting in good
  faith. [Tests](../test.md) names the form agents use.

The whole report is cached per grant and resent verbatim. Engram refuses a
report whole, and evidence adds ways to be refused, so a refused report that
carries checks falls back once to the turn's own observation alone, judged
against its begin-time basis as before checks were reported; a refused
fallback, or a refused report without checks, is dropped, as for the turn's
own observation. The refusals the evidence itself adds are
`environment_fingerprint_mismatch` (a fingerprint its components do not
give), `environment_evidence_not_found` (a reference to no environment
record) and `environment_basis_mismatch` (an environment recorded under
another capability-map revision, or cited from another run or source
revision). TermAl keeps no list of them: the fallback is decided by the
kind of error alone, an answer from Engram as against a lost call, and
never by the message text, so these three take it like any other refusal
and a lost call naming one keeps the evidence for the retry. The stable
code is shown on the checkpoint's control card.

Refuse, defer, protocol/transport degradation, missing binding, begin refusal,
or dispatch-budget exhaustion withhold the prompt and produce a durable Engram
control card. Turning the premium flag off fences the transition, checkpoints
open control state, clears the binding, and resumes ordinary Base-only
dispatch.

### Authorization timeout and retained prompts

Ordinary gated admission uses one twenty-second remaining-time budget across the
work-binding read, binding, evaluation, begin, and host persistence
acknowledgements. Base
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

The default timeout for an individual control call is also twenty seconds (or
the configured project call timeout, which may not exceed twenty seconds).
Outside admission this bounds each completion,
project-reset or stale-begin checkpoint independently; these calls do not share
the admission deadline. A completion checkpoint can
therefore delay the next queued turn by that call timeout. Admission itself still
uses one shared twenty-second budget, not twenty seconds per step.

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
recovery. A Save shares one twenty-second inspection deadline, starting at the
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

## Obligation waivers

Obligation waivers are not a host operation. Engram removed the host-private
`obligation_waive` control call together with resource leases and the
finalizer turn; an operator waives an obligation with the Engram CLI
(`waive-obligation`), and TermAl neither forwards nor audits such a waiver.

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
   exceeds its 20-second bound, leaves the set unknown and refuses nothing:
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
`{ mode, workRef, acceptanceBasis, evidenceBasis, sourceFingerprint?, brief }`,
where the brief tells the caller to record the evaluation with its own tracker
tool, including the `source_fingerprint` when there is one.
`sub_agent` returns `501`.

**Declared source fingerprint.** After the reads, and only for the two modes
that use it (never for `sub_agent`), the host takes the
[content revision](#content-revision) of the worktree the evaluator reads:
the work's named [source root](#source-root) when the requesting session is
bound to the claim that named it (the evaluator child then runs there), else
the parent's worktree (it runs in the parent's workdir). The value
comes from the same function as every basis the turns report. The capture
runs on its own thread and is abandoned at the freeze budget, so a slow file
read cannot hold the request past the allowance the bridge gives it; an
abandoned capture counts as not taken. Its thread lives on until its read
returns. At most four such capture threads are alive in the host at once,
healthy and abandoned alike and across all projects; past that a request
starts none and declares nothing. So a worktree whose reads stall costs at
most four threads however often it is asked, and, while they stall,
evaluations in every project declare no fingerprint. An evaluator
delegation keeps it on its target and declares it as `--source-fingerprint`,
with no workspace. A `same_session` caller gets it in the response and in its
brief. Engram voids an evaluation when the run reports a source change after
it, unless the change is to the declared revision. So a change the parent
reports for the content that was judged, at its turn's close, a commit of
that content, or a peer worktree holding it leaves the evaluation fresh
(tm-5gi4). A change to other content still voids it, and so does a report
of any other revision. When the revision cannot be taken, nothing is
declared, as before, and the response's `notice` says so. An evaluator's
first submission takes the revision again. When the worktree no longer
holds the requested revision, the submission is refused with `409`, naming
both values, before anything runs or is written. The refusal is final for
that evaluator, and both the refusal and the submit tool's description tell
it not to submit again but to finish and report, since only the parent can
request a new evaluation. The parent's fan-in does not yet name this reason.
When it cannot be taken again within
the budget, the submission declares nothing. A replay after an unknown
outcome resends its stored arguments unchanged. In `same_session` the host
measures the value and the agent passes it on; the host cannot see what the
agent passes. Under the policy `require_source_freshness`, Engram's
`done` must also present the evaluated fingerprint; TermAl does not supply
it at completion yet.

**Read budget.** Every tracker call runs through the one-retry lock policy, so
it can cost two command timeouts plus the retry delay. One function computes
the worst case of a request (the two task reads, up to seven continuation
pages, the policy read and the source capture, bounded by the freeze budget)
and both sides use it: the MCP bridge adds it to its HTTP allowance, and the
request path takes it as its own deadline. Before each continuation page the
host checks that the deadline still funds that page, the two reads that decide
the request and the capture; when it does not, paging stops and the brief
lists the evidence read so far. The bridge therefore never gives up on a
request the backend is still serving.

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
keeps the newest 40 entries, each with its body whole, and states how many
older ones are not shown; an entry the tracker would refuse as a citation (a
non-holder observation, a restored-record member) is marked as context only.
The evaluator may call no tracker tool, so what the brief does not show it has
not read, and the brief never cuts a record silently. When the brief must
shrink, context gives way first and in this order: the listed entries are
clipped to their first 600 characters, oldest first, each ending in
`[clipped by the host: SHOWN of TOTAL bytes shown; locator LOCATOR]` (the
sizes are those of the one-line text); then entries are left out, oldest
first, down to none with the outcome still at its own bound, and the line that
counts them names the ones the host had read, `(N older entries not shown; the
host read and left out M: LOCATOR, …)`; only then the outcome, down to
its marker alone (an outcome shorter than the marker is never traded for it).
A record the tracker's own window listed without its body or with a cut one
(`body_omitted`, `summary_truncated`, or no text at all) ends in
`[not shown in full by the tracker: TOTAL bytes stored; locator LOCATOR]`.
The brief's rules tell the evaluator that such an entry is incomplete and that
a verdict depending on it is `insufficient-evidence` naming the locator. The
request's answer tells the requester the same in its `notice`: which entries
the host clipped, which it left out, which the tracker's window did not give in
full, and how many older ones were never read, naming at most 64 locators per
group. It says nothing when every record is carried whole.
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
  [--source-fingerprint content-v1:<sha256>]
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
The captured setter help fixture records build `df134a518b60` (2026-09-23);
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
Read and write process calls have a twenty-second
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
routing token, an active grant, an uncertain grant or a rebind marker,
including roots that have never created a delegation. Never-bound sessions
are not eagerly bound or readiness-fenced: their first turn performs normal
lazy admission. A child of a delegation that already ended (completed, failed
or canceled) is skipped unless it still holds a mirrored or uncertain grant,
which recovery checkpoints or clears as before. That skip is safe because
every grant that may be open on Engram is recorded locally:

- **What is recorded.** A begun turn is mirrored when its dispatch record
  commits (a failed turn-end checkpoint keeps it). A begin whose outcome never
  arrived is recorded as uncertain: abandoned in flight by a stop or a
  cancellation, failed in transport, answered for another grant, or its
  durable queued intent dropped by a cancellation after a restart (then only
  an unknown-grant marker). A live runtime marker naming the exact grant wins
  over the intent; an intent this process issued whose marker sent no begin
  records nothing, so only a restored or interrupted intent can. Retiring a
  promoted head for a terminal callback or a Stop records the intent it
  carried first, and a dispatch record keeps a begin its released marker
  still names when it mirrored nothing.
- **What settles it.** Only the status-authoritative paths: a clean session
  status or an accepted fresh bind clears it; an open grant reported by
  status is checkpointed and cleared on a receipt that names it (a receipt
  for another grant settles nothing, and recovery asks again later); a begin
  that completes for the still-current dispatch mirrors it as begun; and the
  released begin's own compensating checkpoint clears it on a receipt or
  keeps it on failure, taking a settled begin off its marker. Turn-end, stop
  and reset checkpoints ignore it. The unknown marker is cleared by a clean
  status or by the accepted bind that follows a checkpoint.
- **Project reset.** On the same authority store the reset keeps an uncertain
  grant, a record whose begins may be unrecorded, or retained queued intent
  that may be the only evidence of a begin, with the token that can ask about
  it, for the session's next bind to settle; a home change drops it with the
  binding.
- **Records written before begins were recorded.** A failed or canceled
  child loaded from such a store may hide a begin, so it keeps being
  recovered, after the live sessions, until one accepted bind settles it and
  marks it recorded; each bounded boot settles as many such records as its
  budget allows. A completed child's turn ran, so its begin was mirrored
  before the outcome, and it stays skipped whatever its record says.

The skipped child's stale token stays in place for the ordinary rebind path
of a later follow-up. TermAl publishes
`engramBootRecoveryPending` before recovering bindings, bounds the overall
work by `bootRecoveryBudgetMs`, and retries an unfinished target lazily on the
next targeted read or prompt. Base MCP/context injection does not bind a
control session and never withholds delivery.

Recovery diagnostics keep the stable single-line form
`boot-recovery session=<id> command=<phase> attempt=<n> elapsed_ms=<n>
outcome=<ok|error>`. Phases include `session_status`, `turn_checkpoint`, the
work-binding read (`work_core_held`), `session_bind`,
and the whole target. The coordinator emits
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
