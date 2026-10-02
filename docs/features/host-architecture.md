# Host Architecture: Event Stream, Engram Host Split, Owned State, Process Supervision, Execution Record

See [architecture](../architecture.md) for the system as a whole. This brief
changes how six existing references are built, and each links back here:
[Engram host adapter](./engram-host-adapter.md),
[shared live events](./shared-live-events.md),
[file change awareness](./file-change-awareness.md),
[workspace terminal](./workspace-terminal.md),
[Windows command containment](./windows-command-containment.md) and
[test runs](./test-runs.md). It is listed in the
[feature index](./README.md).

## 1. Status, scope and terms

**Status: proposed.** This is a design brief. It contains no product code and
changes no behaviour. It describes the backend as it stands on `master` at
commit `28e9ed5` and the structure the backend moves to.

**Why now.** New feature work in the Engram host area (the adapter, turn
checks, carried checks, acceptance evaluation) is frozen until this brief is
agreed. Bug fixes with regression tests continue. The project's coordinator
decided the freeze on 2026-09-30, with the concurrence of the sessions then
working in that area, and the decision is recorded in the project's tracker.
The freeze exists because
each recent feature there needed changes in about a dozen unrelated files
(section 2.4), and because the defects found this month share a small number
of structural causes (section 2.9).

**What it covers.**

- (a) One host event stream for recorded observations, and the contract of the
  client stream beside it (section 4).
- (b) The split of `src/engram_host_adapter.rs` by ownership, and the contract
  TermAl keeps with Engram (section 5).
- (c) State owned by one component each, behind a handle (section 6).
- (d) One owner of process launch and supervision (section 7).
- (e) One execution record (section 8).
- The first extraction, its later steps, and the tests and proofs that
  constrain each (sections 9 and 10).

**What it does not cover.**

- The browser's adoption of snapshots and hydration responses. Section 4.11
  defines only what the server guarantees.
- Any change to Engram's wire protocol. The split changes no byte sent to
  Engram. Contract fields TermAl does not send yet are listed as such.
- Remote-proxy sessions, which never enter either Engram tier.
- A change to the rules that decide whether a check is credited. Those rules
  move behind a boundary first and change later, in a step of their own.

**Terms.**

- **Observation.** A fact the host saw and records: a command started, a file
  was written, a snapshot finished. An observation never grants anything.
- **Authority operation.** A call that changes who may do what, or what counts
  as evidence: binding a session, admitting a turn, closing a grant, naming a
  root, recording a verdict. It is a direct call on its owner. An event
  alone never carries one out (section 4.6).
- **Fence.** The value an authority operation compares before it acts, so a
  late caller cannot act for a turn, runtime or settings generation that has
  moved on: a generation number, a routing token, a grant id.
- **Owner.** The one component that holds a piece of state and the lock that
  guards it.
- **Handle.** The type through which other code reaches an owner: operations
  and immutable snapshots, never fields.
- **Execution.** One run of one command or process, with an identity that does
  not change (section 8).
- **Check.** A recognised test command that ran during a mediated turn, with
  the source snapshots taken around it. A **carried check** is a full gate
  launched in the background whose result arrives after its turn.
- **Cut.** A point in the event stream together with the statement that a
  consumer has applied everything admitted before it (section 4.7).
- **Provisional / durable.** A record is provisional while it exists only in
  memory. It is durable once the persistence writer has acknowledged exactly
  that record.

## 2. Current shape

Every statement in this section was read from the source at `28e9ed5`. Line
numbers pin a claim; function names locate it.

### 2.1 One module

`src/main.rs` builds the backend from 150 `include!()` fragments. There are
four real modules: `tests`, `windows_folder_picker`, and two that the
Windows containment change added, `host_command` and `windows_launch`, each
with a `pub(crate)` surface.

All feature code, the whole Engram host area included, is therefore one Rust
module. Every function, field and constant is visible from every file. A new
file is a place to put code, not a boundary:

- Outside the `engram_*` fragments and the tests, a field of a session's
  Engram state is read or written directly 118 times in 9 files.
  `src/session_crud.rs` has 57 of them and `src/turn_dispatch.rs` has 27. The
  SSE publisher has two (`src/sse_broadcast.rs:580` and `:582`,
  `active_turn_source_root` and `active_grant_id`).
- The tests do the same 804 times in 27 files.
- The count is of matches of the regular expression
  `\.engram\s*\.\s*(FIELD)\b` over `src/**/*.rs`, where `FIELD` is the
  alternation of the 49 field names of section 6.2, matched across line
  breaks so that a field on the line after `.engram` counts. A project's
  Engram settings (`project.engram`) have other field names and are not
  counted.
- 29 call sites in 9 files outside the fragments report a fact to the check
  machinery (section 2.4).

### 2.2 The Engram host area

| Fragment | Lines | What it holds |
| --- | ---: | --- |
| `engram_host_adapter.rs` | 7,020 | Settings and diagnostics, wire types, test transports, session state, boot recovery, checkpoint, admission, bind, evaluate |
| `engram_source_roots.rs` | 1,748 | Named source roots: validation, naming, host lines |
| `engram_turn_checks.rs` | 1,736 | Check lifecycle, overlap, report layout |
| `engram_carried_checks.rs` | 1,646 | Background and detached gates carried past their turn |
| `engram_queued_admission.rs` | 1,181 | Retained bind and evaluate on queued prompts |
| `engram_check_paths.rs`, `engram_check_recognition.rs`, `engram_write_places.rs`, `engram_one_call.rs`, `engram_check_toolchain.rs` | 3,056 | Which commands are tests, where they ran, where they may write |
| `engram_turn_observations.rs` | 664 | Source basis at begin and close, report fallback |
| `engram_mcp_config.rs`, `codex_engram_bootstrap.rs` | 830 | Base tier: MCP composition, orientation read |
| `engram_control_transport.rs` | 461 | The control sidecar process per session |
| `engram_readiness.rs`, `engram_session_reconciliation.rs`, `engram_work_binding_refresh.rs`, `engram_held_claims.rs`, `engram_evaluation_refusal.rs` | 1,361 | Readiness, absence inspection, work-binding reads |

In `engram_host_adapter.rs`, one `impl AppState` block runs from line 3100 to
line 6879. The scripted and stateful test transports (`#[cfg(test)]`, lines
979 to about 1600) sit in the same production file.

The `engram_*` test files are 46,284 lines in 26 files;
`src/tests/engram_host_adapter.rs` alone is 16,627.

### 2.3 One struct, seven concerns

`EngramSessionState` (`engram_host_adapter.rs:2211`) has 49 fields and lives
on every `SessionRecord`, under the global state lock. Section 6 lists every
field. They belong to seven concerns:

1. turn admission and the control binding (17 fields);
2. transport health (3);
3. verification: checks, carried checks, the report cache, the checkpoint
   claim (12);
4. where the session's commands run: running commands, their worktrees, the
   shell's presumed directory (4);
5. the turn's source root and the host lines waiting for the agent (3);
6. the settings-transition fence (1);
7. the Base tier's orientation read (9).

Further Engram state sits on `SessionRecord` (six more fields), on
`StateInner` (ten), on `AppState` (`engram_carried_poll_lock`), on each
`QueuedPromptRecord` (four retained-intent fields,
`session_interaction.rs:163` to `:169`), and in two client views derived at
serialisation (section 6.3).

### 2.4 Case study: one feature, twelve existing files

The check machinery learns what happened from direct calls. At `28e9ed5`
there are 29 such call sites in 9 files outside the `engram_*` fragments:

- `recorders.rs` (7) reports command starts, ends, descriptions,
  abandonments and edits;
- `turn_dispatch.rs` (5) and `session_lifecycle.rs` (1) report turn starts
  and the session's worktree;
- `terminal.rs` (4), `api_git.rs` (6), `api_files.rs` (2) and
  `api_review.rs` (2) report host writes;
- `workspace_watch.rs` (1) reports each watcher event;
- `test_runs.rs` (1) polls carried runs on its tick.

Each of those call sites knows that checks exist. None of them needs to: each
only has a fact to report.

The change that credits background gates (`76236ec`) shows the cost of the
next feature. It added one module and its tests, and to work it also had to
change twelve existing source files: the last two call sites above
(`workspace_watch.rs`, `test_runs.rs`); the checkpoint in
`engram_host_adapter.rs`; `engram_turn_checks.rs`,
`engram_check_recognition.rs` and `engram_source_roots.rs`; and
`persisted_state.rs`, `session_crud.rs`, `session_lifecycle.rs`, `state.rs`,
`app_boot.rs` and `main.rs` for its state, its reset and its lock.

### 2.5 The state lock and the client stream

- **Revision and publication are two steps.** `push_message`
  (`session_messages.rs:51`) takes the revision under the state lock (`:91`),
  releases the lock, and then calls `publish_delta` (`:103`). So do
  `insert_message_before` (`:184`, `:196`), `append_text_delta` (`:292`),
  `replace_text_message` (`:387`), `upsert_command_message` (`:533`, `:549`,
  `:562`), `upsert_parallel_agents_message` (`:669`, `:685`, `:698`) and
  `finish_engram_checkpoint_record` (`engram_host_adapter.rs:4129`). Two
  threads can therefore queue deltas in an order other than their revisions,
  and a snapshot, which is queued under the lock, can overtake an older
  delta. The client ignores a delta at or below the revision it holds.
  These are the sites this brief read, not a complete list. Outside the
  tests, `publish_delta` and `publish_message_created_delta_parts` are
  called at about 60 places in 17 files (`delegations.rs` 13,
  `remote_delta_apply.rs` 12, `session_messages.rs` 8, and fourteen more
  files). They were not each checked for where their revision is taken, and
  the step that changes this (section 4.11) has to size itself on all of
  them.
- **The comment says otherwise.** The doc comment on `publish_delta`
  (`sse_broadcast.rs:433`) says callers follow it with a commit "under the
  same lock".
- **The broadcaster drops silently.** `StateBroadcastMailbox` holds at most
  256 pending items (`state.rs:409`) and discards the oldest when full
  (`:427`, `:440`), with no log, counter or marker. This happens before the
  broadcast channel, so no client receives `lagged`. A client learns of the
  loss only from a revision gap, and only if a later event arrives.
- **Two consumers run per watcher event under the state lock.** The watcher
  thread calls `record_active_turn_file_changes` and
  `note_engram_workspace_file_changes` for every notify event
  (`workspace_watch.rs:66`, `:67`), before its 250 ms coalescing (`:19`).

### 2.6 Check facts the design must keep

- A check stays open to writes until **both** its snapshots are ready, the one
  taken at its start and the one taken at its end
  (`EngramTurnCheck::open_to_writes`, `engram_turn_checks.rs:800`). Readiness
  is set by the capture's own thread, outside the state lock.
- A check is overlapped at its start when another command of the session
  runs, when another writable session is in a turn in the same worktree, or
  when the root was named after work began (`:1121`,
  `engram_other_writer_in` at `:739`).
- A worktree the host could not name counts as every worktree
  (`engram_worktrees_may_hold`, `:725`).
- Marks that arrive while the checkpoint waits for snapshots off the lock are
  merged in when it takes the lock again (`engram_merge_live_overlaps`,
  `:512`).
- The checkpoint takes the settled carried checks, validates their fences,
  removes them and composes the report in one critical section
  (`checkpoint_engram_turn_off_lock`, `engram_host_adapter.rs:3812`;
  `engram_credit_carried_checks`, `engram_carried_checks.rs:1230`).
- A session remembers the write places of at most 64 running commands
  (`ENGRAM_RUNNING_COMMAND_LIMIT`, `engram_turn_checks.rs:539`). Past that
  the oldest is
  forgotten and stops counting as a writer. This is the one limit in the
  area that errs toward credit; it is tracked as a defect (section 13).

### 2.7 What TermAl sends Engram, and what it does not

- One report per grant, built at the first closing attempt. Its idempotency
  key hashes the report (`engram_checkpoint_idempotency_key`,
  `engram_host_adapter.rs:6921`).
- A refused report falls back in three stages: the report, then the turn's own
  observation, then nothing (`forget_refused_engram_turn_report`,
  `engram_turn_observations.rs:489`). Any refusal from Engram is eligible.
  The holder sees a degraded card with the refusal code; nothing names the
  checks whose evidence was lost.
- The checkpoint receipt is decoded into `grant_id` and two cursors
  (`EngramCheckpointReceipt`, `engram_host_adapter.rs:796`). Engram's receipt
  also returns the ids
  of the observation, verification and environment records it stored. TermAl
  discards them.
- The only verification kind is `Test` (`engram_turn_checks.rs:65`, `:1680`).
- A source basis carries `workspace_id` and `source_revision` only
  (`EngramExecutionSourceBasis`, `engram_host_adapter.rs:668`).
- TermAl does not send `named_root_bind`. A named source root is host state;
  Engram sees it only as a `workspace_id`.
- TermAl does not send `reported_source_change`, although it already
  distinguishes the three cases that field names.
- `engram authority revoke` is still built at
  `engram_host_adapter.rs:1798`. Engram has no such
  command. The path can run only for an unconfirmed entry of a persisted
  ledger that current configurations cannot create; it is dead code (section
  13).

### 2.8 Process launch

Standard host launches use `host_command::Command`; native terminal and
bounded-read launches use `windows_launch::prepare`. Both share the Windows
inheritance guard. The Engram control sidecar still uses its existing
process-tree transport (`EngramProcessTree`, `engram_host_adapter.rs:1598`,
which wraps the terminal's `TerminalProcessTree`) and not the native
primitive; agent runtimes retain their existing shutdown ownership. A shared
guard alone establishes no containment guarantee. Section 7 has the
ownership table.

### 2.9 Six live defects

| Defect | What happens | What it shows about the structure |
| --- | --- | --- |
| A false refusal after a background launch | After `pushd "DIR" && …` launches a gate in the background, the holder's later commands are placed in the gate's worktree and refuse its credit | Where a session's commands run is a side effect of check bookkeeping, not a fact of its own |
| Scratch writes invalidate a carried gate | The watcher fence counts writes under `.tmp/`, where every agent is told to put scratch files | Each consumer of file changes applies its own idea of what a source change is |
| One session's unplaced command refuses every carried gate on the host | A writable session with a shell or command the host cannot place counts as a possible writer in every worktree, so gates carried by other sessions in other worktrees lose their credit | An unknown place becomes "everywhere", and "may have run there" becomes "wrote there": an observation is treated as a write |
| A session that holds two claims is measured for only one | The host resolves one bound claim per session (the focused one, else the current, else the newest: `select_engram_held_binding`, `engram_held_claims.rs:167`) and uses it for the turn's measured root, for test credit and for an acceptance request (`acceptance_evaluation_api.rs:763` to `:779`, `engram_evaluation_source_root` at `engram_source_roots.rs:349`). A test run in the other item's root is refused, and an evaluation of the item the session is not bound to takes the session's workdir and the other item's claim | The session's one binding stands in for the work a measurement is about |
| A refused report loses evidence silently | Section 2.7 | The receipt is not kept, so the host cannot compare what was stored with what it sent |
| The launcher's input fingerprint is unstable on CRLF checkouts | A later recompute differs from the gate's recorded value | Outside this brief; listed because carried credit depends on it |

The first and third are the execution-record problem in small: where a
command ran, and whether it wrote, are facts the host should record once and
keep, not conclusions each check draws again. The second is the
single-stream problem in small. The third was reported while this brief was
being written, when two sessions' gates were refused with a line naming a
third session that had run no command in their worktrees. The fourth was
reported the same way, by two sessions that each held two claims; rule 10 in
section 3 is its answer. The rows state
each defect as its item reports it. The third column is this brief's reading
of what the defect shows about the structure, not an established cause: the
cause of the false refusal, for one, is inferred from source on its item and
not yet proven by a test. Owned state explains why the structure should
change; it is not evidence that private fields fix shell semantics.

## 3. Rules

The rest of the brief follows these.

1. **Authority operations are direct and fenced.** They are calls on the
   owner, they compare a fence, and they return a typed result that
   distinguishes done, refused and unknown. An observation is never the
   authority for one. A projection performs none. A component that an
   observation wakes performs only the fenced operation it owns itself, and
   only after verifying the receipt the relevant owner holds (section 4.6).
2. **An observation is recorded once, at the fact.** It takes its sequence
   number in the critical section that decides the fact. Consumers run later,
   on their own threads, outside every lock.
3. **One owner per piece of state.** Other code reaches it through the
   owner's handle.
4. **No file, process or network I/O under the state lock or an owner's
   lock.** This is already the rule; the split must not weaken it.
5. **One lock order.** The state lock or one owner's lock, then the event
   log's lock, which is a leaf: nothing is acquired or called while it is
   held. No new owner's lock is taken while the state lock or another
   owner's lock is held. No recording occurs while the launch guard is held:
   pre-launch admission finishes before acquiring it, and creation
   observations are recorded after releasing it. The nestings that exist
   today and are kept until a lock migration of their own are listed in
   section 6.5.
6. **Durability follows need.** An intent that must survive a restart before
   an external call is made durable first, and the call waits for that
   acknowledgement. An observation of something that already happened may be
   provisional. Nothing may depend on a provisional record for authority or
   credit beyond what the code at `28e9ed5` already does.
7. **Uncertainty withholds.** When the host cannot tell, a check is unknown
   and a credit is not given. No step may make that less conservative without
   saying so and proving it.
8. **An event is a wake for an authority consumer and a fact for a
   projection** (section 4.6).
9. **Moves and changes are separate commits.** A commit that moves code
   changes no behaviour, renames nothing and changes no signature beyond the
   visibility the move needs.
10. **Every work-scoped measurement and evidence record names the work it is
    for.** Its authority and attribution are resolved from that work's own
    claim and root, never from whatever the session happens to be bound to
    at the moment. A request that names a held work the turn is not bound
    to is refused with the rebind it needs or, for an evaluation, resolved
    under that work's own claim; it is never carried by the bound work's
    grant in its place (section 5.4). A generic host or execution
    observation may stay unattributed, or be linked to several separately
    attributed executions; it invents no claim and no credit.

### 3.1 Real modules or fragments

This subsection is the one decision in the brief that reverses a stated
trade-off, so it is written to be read without the rest.

**The situation.** The backend is built by pasting 150 source files into one
Rust module (`include!()` in `src/main.rs`). `main.rs` and
`.claude/reviewers/architecture.md` call that intentional: every type is
visible everywhere and no file needs export declarations. The price is that
no file can keep anything to itself. A session's Engram state has 49 fields;
code outside the Engram files reads or writes them directly in 118 places
across 9 files, and the tests do so 804 times. When a feature needs a new
fact about a session, the shortest path is one more field and one more direct
access, and the last such feature changed twelve existing files (section
2.4). Rule 3, one owner per piece of state, can be kept by the compiler or
by convention.

**Option 1, recommended: real Rust modules for the new owners.** The event
stream, the execution record, the process supervisor and the Engram host
become modules with private fields and a `pub(crate)` surface. Existing
fragments stay as they are until a step moves them. Nothing is rewritten at
once.

- *What it gives.* A module's private fields cannot be read from outside it.
  A child module still sees everything in the crate root, so a component can
  use `AppState`, `SessionRecord` and the wire types freely while the root
  sees only what the component exports. The list of operations other code
  may call (section 5.2) is then a list the compiler enforces.
- *Migration cost.* Every function other code calls must be marked
  `pub(crate)`; that list is the facade. The 118 direct field accesses must
  become calls, a few at a time, as each field becomes private. The 804
  accesses in tests either move with their test files into the component
  (`engram_host::tests`, where private access is kept) or go through
  test-only accessors. The first is a pure move and is preferred.
- *Effect on the `include!()` build.* None for existing fragments. A module
  can also be assembled from existing fragment files (`include!` inside the
  module's file, with `use super::*;`), which gives the boundary before any
  file is rewritten. That tactic has not been compiled for this brief.
- *Precedent.* `host_command` and `windows_launch` are real modules with a
  `pub(crate)` surface on `master` today.
- *Reviewer guidance.* `.claude/reviewers/architecture.md` would add one
  check: flag a new fragment, or a new field access from outside, for state
  that a component owns. Its statement that one large `main.rs` is an
  accepted trade-off stays true for the code not yet moved.

**Option 2: fragments only, ownership by convention.** The adapter is split
into more fragment files and a comment names each file's owner.

- *What it gives.* Smaller files and no migration of tests or call sites.
- *What it costs.* Nothing prevents the next feature from reading another
  component's fields, which is how the twelve-file change came about. A
  handle that can be bypassed is documentation. Reviews would have to find
  each bypass by reading.

**Consequence of each.** With option 1 the work of sections 9 and 10 ends
with boundaries that stay in place without anyone watching them, at the
price of moving test files and converting field accesses step by step. With
option 2 the same steps are shorter and their result lasts only as long as
every later change respects it.

The brief is written for option 1. Under option 2, sections 5 and 6 keep
their ownership tables and lose their enforcement.

## 4. The host event stream

### 4.1 What it is

An in-process, ordered, bounded log of observations, with consumers that read
it by cursor. It exists so that a part of the host that has a fact to report
(a command ended, a file was written) records it once and knows nothing of
who uses it.

It is per process. A restart starts a new **epoch** with an empty log. It is
not a journal of record: the durable facts are the execution record (section
8), the persisted session state, and Engram's own store.

It is not the client stream. `/api/events` carries revisioned state for
browsers and keeps its own contract (section 4.11).

This section describes the target. The first extraction builds the log, the
recording and a projection that decides nothing (section 9); consumers take
over from today's direct calls in the later steps of section 10.

### 4.2 The envelope

```rust
struct HostObservation {
    epoch: HostEpoch,            // changes at every process start
    sequence: u64,               // order of recording within the epoch
    admission: AdmissionId,      // always present; pairs Admitted with Resolved
    execution: Option<ExecutionId>,
    session: Option<SessionId>,
    runtime_generation: Option<u64>,
    turn_generation: Option<u64>,
    recorded_at: Timestamp,
    kind: ObservationKind,       // typed payload, facts decided at recording
}
```

`sequence` means order of recording and nothing else. It is never an
identity and is never compared across epochs. The identity of an execution's
transition is `(execution id, transition version)` (section 8).

### 4.3 Catalogue

In the target, the observations below replace today's direct calls. "Decided at the fact" is
what the payload must carry, because a consumer reading later cannot
reconstruct it from current state.

| Observation | Recorded by | Decided at the fact | Today |
| --- | --- | --- | --- |
| Turn started | turn dispatch, under the state lock, before the prompt reaches the runtime | session, generations, writer's worktree (resolved off the lock just before), the turn's root | `engram_note_turn_started` |
| Turn ended | turn lifecycle, under the state lock, at the transition that ends the turn | session, generations, how it ended | read from the session's status |
| Command event admitted | the recorder, in the critical section that reads the session's generations, appended straight to the log under the state lock; not a transition of an execution | the admission id, the session and its generations, and a reserved execution id when there is one | none |
| Command started, described, finished, abandoned | the execution owner, as transitions of an execution, after the state lock is released; each cites its admission | the session's running set at that moment, resolved write places, shell position, launch-only disposition, typed result | `note_engram_command_*` |
| Workspace edit reported | the recorder | session; an edit names no path | `note_engram_workspace_edit` |
| Host write started, ended | terminal, Git, file and review routes | path or repository root | `note_engram_host_write` |
| Watcher batch admitted, resolved | the watcher thread: admitted at the first notify event of a pending batch, resolved when the batch is published | at admission, that a write was seen and not yet where; at resolution, the paths; no trustworthy time | two calls per notify event |
| Snapshot ready | the capture's own thread | which check, start or end, the revision or its absence | readiness flag read at mark time |
| Launcher record first read as terminal | whichever pass reads it first | run directory, digest of the terminal record | the carried check's pin |
| Test run settled | the test-run index tick | run, verdict | index state |
| Checkpoint committed | the verification owner, after Engram's receipt | grant, receipt, which checks were stored | none; callers poll |
| Root bound, root ended (target) | the source-root owner, after a confirmed receipt | claim, generation, workspace | none |
| Launch prepared, started, failed; cancellation requested; root exit observed; cleanup settled; output settled | the process supervisor (section 7) | per section 7 | none |
| Execution terminal recorded; observation continuity lost | the execution owner (section 8) | per section 8 | none |

Pipe chunks, streamed text and other high-volume data are not observations.

### 4.4 Recording

Recording is synchronous with the fact. The recording call takes the log's
lock, assigns the next sequence number, appends, and returns. It does no I/O
and calls no consumer.

Where a fact is decided under a lock, the observation is recorded before that
lock is released: the state lock for a turn start and for the admission of a
command event, the execution owner's lock for a transition, a capture's own
mutex for snapshot readiness. For one owner, the order of observations is
therefore the order of its commits.

Some observations cannot be complete at the moment they are decided: a
command's write places are resolved on the file system, off the lock. These
are recorded in **two phases**:

- **Admitted**, in the critical section that reads the session's generations.
  It has its sequence number, its admission id and its generations, and its
  places are not yet known. That section is under the state lock, so the
  admission is appended straight to the log, which the state lock may hold
  (section 6.5). It is not a transition of an execution and takes no
  owner's lock.
- **Resolved**, after the off-lock work, citing the admission id. For a
  command this is the execution owner's transition, committed under its own
  lock after the state lock has been released.

An observation that is admitted and not yet resolved is explicit. It counts
as a possible write in every worktree, which is how an unnamed worktree
counts today. A stale generation never removes an observation's effect on
other sessions; the generation decides only which turn the observation
belongs to.

The watcher is recorded in the same two phases, because it coalesces. Today
it tells its consumers of every notify event at once and publishes a batch
only after 250 ms of quiet, which continuous events can postpone without
limit. If consumers read only published batches, a write the watcher has
already received would be outside every cut until its batch is published,
and a checkpoint could credit a check that write had touched. So the watcher
admits a pending batch when it accepts that batch's first notify event,
synchronously and before the event enters any coalescing queue, as one
append to the log and without the state lock. The watcher records Resolved
with the sealed batch's immutable facts, referencing its admission, without
waiting for a consumer. Each consumer installs those facts before advancing
its resolution watermark; a cut is `Complete` only after that installation.
While a batch is admitted and unresolved it counts as a possible write in
every worktree, a cut through it is `Incomplete`, and the checkpoint waits
for it within its budget or withholds. A batch has a maximum age measured
from its first event, not from its last. Under continuous events the watcher
seals the batch at that age and gives later events a new admission; neither
the seal nor a new arrival clears the other's pending accounting, and
unresolved admissions survive eviction in the gap summary. Admission starts
when the watcher thread dequeues the event: an event notify has delivered
to the watcher's channel (`workspace_watch.rs:31` to `:34`) and the thread
has not yet dequeued is outside every cut, which is why an operation that
composes from a cut drains the channel with a marker before it seals
(section 4.7), and why a write notify has not yet delivered at all is stated
there as the residual. This belongs to the steps that make the cut
authoritative and move the consumers (steps 10b and 12); it withholds
nothing in the first extraction.

The admission id is always present. The execution id is optional until the
execution owner has resolved which execution a provider's report belongs to.
A repeated observation of the same invocation creates no new execution; a
new invocation that reuses the key does.

### 4.5 Order

Guaranteed:

- One total order per epoch. If one recording's critical section ended before
  another's began, its sequence number is lower.
- For one execution, stream order equals the order of its transition
  versions.
- A turn-started observation precedes every observation admitted under that
  turn generation.

Not guaranteed:

- That the order of recording is the order in which things happened outside
  the host. A runtime reports a command after it began. The watcher reports
  late. Consumers treat a watcher batch as open to the left: it may describe a
  write from any time before its recording.

### 4.6 Consumers

Each consumer has its own cursor and its own thread. A consumer that is slow
or stuck delays nobody else, and never delays a recording.

There are two kinds, and the difference is a rule:

- **A projection** builds derived state from the facts in the observations:
  which checks are open, which writes touched them. It acts on the immutable
  payload, or fetches the exact `(execution id, version)` record. It never
  rereads current mutable state to reconstruct what was running, where, or in
  which phase at an earlier moment.
- **An authority consumer** is an owner that an observation wakes. It uses
  the observation only as a wake: before it acts, it verifies the exact
  receipt the relevant owner holds, and what it then performs is its own
  fenced operation, never another component's. The evaluation hold is the
  first. It owns the deferred evaluations. A "checkpoint committed"
  observation tells it to look, and it releases a deferred evaluation only
  when the verification owner holds the checkpoint receipt for that exact
  turn and grant (section 10.1). The release is a fenced operation of the
  hold, listed in section 11; where it starts an evaluator or queues a
  prompt, it does so through the ordinary fenced operations for those acts.
  After a gap or a restart the hold reconciles against the owner or keeps
  holding, and a bounded fallback means a lost wake cannot strand it.

Every consumer is idempotent. Delivery is at least once; a consumer
deduplicates on `(execution id, version)` for transitions and on
`(epoch, sequence)` otherwise.

A projection calls no authority operation. An authority consumer calls only
the fenced operation it owns, after the verification above. No consumer
waits for another consumer, and no authority operation waits for an
authority consumer.

### 4.7 The cut

Some authority operations need to know that a consumer has applied everything
up to a point. The closing checkpoint is one: every overlap mark recorded
before it must be applied before the report is composed.

```rust
struct StreamPosition { epoch: HostEpoch, sequence: u64 }

enum Cut {
    Complete { admitted_through: StreamPosition, resolved_watermark: StreamPosition },
    Incomplete { unresolved: BoundedList<AdmissionId>, more: usize },
    Gap { from: StreamPosition, to: StreamPosition },
    DeadlineExceeded,
}
```

- The caller reads the current position `N`, epoch included, and asks a
  consumer for a cut through `N`, with a deadline, holding no lock. For an
  operation that composes from the cut, `N` is the position of its own seal
  (below), not a position read earlier. A
  position from another epoch is a `Gap`; sequence numbers of different
  epochs are never compared.
- `Complete` means two things. Every observation admitted at or before `N`
  has been resolved, and the consumer has installed the state of all of
  them, including resolutions recorded after `N`. A bare "processed through
  `N`" is not enough: a recorder that read its generations before `N` and
  appended after it would be missed.
- `Incomplete` names the admissions still unresolved. The caller withholds
  whatever they may touch.
- On anything but `Complete`, the caller takes the conservative path. For
  the checkpoint that is what it does today when a snapshot misses its
  budget: what is not ready is withheld.
- The cut is advice to an authority operation, not a lock, and the owner's
  lock fences nothing that ingress appends under the log's lock alone: a
  watcher batch or an execution transition can be admitted between the
  wait and the composition. So an operation that composes from a cut
  **seals** first. The seal is one append to the log, under the log's lock
  and no other, of a seal observation naming the operation; its position
  `S` is totally ordered with every admission, and the operation composes
  over the cut through `S`, not through a position it read earlier. An
  admission after `S` is after the seal by construction and belongs to the
  next cut. Before the seal, and holding no lock, the operation drains the
  ingress that buffers: it posts a drain marker through the watcher's
  channel, and the watcher thread admits the marker after every event it
  dequeued before it, so every notify event delivered to the watcher
  before the marker is admitted before `S`. A watcher batch admitted
  before `S` and unresolved makes the cut `Incomplete`, and the operation
  withholds what it may touch. The drain, the seal and the cut are all
  requested within one deadline; a watcher thread that does not reach the
  marker in time gives `DeadlineExceeded` and the conservative path.
- **The seal follows every included window's close, never precedes it.**
  A check is open to writes until both of its captures are ready (section
  8.5), and the captures can finish in either order. A check is included
  in a composition only when both readiness observations, each recorded
  on its capture's thread under the capture's mutex (section 4.4), are
  installed and have positions at or before `S`; its close is the later of
  the two. A carried run is included only when the observation of its
  settled first terminal read is at or before `S`, under its existing pin
  rule. A check whose close is after `S` is withheld from this
  composition; any later eligibility follows the existing original-grant
  and carry rules, with no rebinding and no new credit promise. So the
  order of a checkpoint's composition is: wait for readiness within the
  budget; drain; seal; cut through `S`; compose under the owner's lock;
  withhold what closed after `S`. The seal moves no check's boundary
  earlier: an observation admitted before `S` is covered by the complete
  cut and the readiness order; a writer admitted after `S` is after the
  close of every included window.
- What the seal cannot cover is stated, not hidden. Admission order is the
  order of the host's knowledge, not of the writes (section 4.5): a
  provider report or a notify event delivered after `S` may describe an
  earlier write. A write whose notify event the operating system has not
  yet delivered to the watcher at the marker is outside the cut, exactly
  as it is outside today's carried fence. Its batch is admitted after `S`
  with an interval open to the left, is applied by the consumers, and
  fences the checks of the next cut; a report already composed and sent is
  not reopened for it, and the loss is bounded by notify's delivery
  latency, not by the host's own queues. A later admission is never read
  as the absence of an earlier overlapping write. Retracting credit
  already reported is a contract change (section 12).
- After the cut, the operation takes its owner's lock only to freeze the
  report against changes of that owner's own state; it does not revalidate
  the stream there, since nothing after `S` belongs to it.

### 4.8 Bounds, gaps and retention

The log is bounded. A consumer that falls off its tail receives a typed gap
naming the range and rebuilds from the owners' state.

- A gap is a monotone marker in the consumer's state. It cannot be cleared by
  rebuilding: current owner state cannot show a write that began and ended
  inside the missing range.
- A gap fences every unresolved check it may intersect. A narrower scope
  needs evidence.
- Pending admissions survive eviction in the accounting.
- An open check or a carried run does not pin log entries, since a run can
  last hours. What is kept for it is a compact summary: that a gap touched
  it, and which writers were active. Eviction can never read as "no overlap".

### 4.9 Restart

A restart changes the epoch. Consumers rebuild from owners. The execution
owner annotates records whose observation was interrupted (section 8); that
annotation is not a terminal result, and a completion that arrives later for
such an execution is a fact about the execution and never a reason to give
credit. Credit stays tied to the grant, root, directory, toolchain and source
basis the execution started with, and what is lost at a restart today, a
carried gate's credit, stays lost.

### 4.10 What is not an event

Root naming, turn admission (bind, evaluate, begin), the checkpoint claim and
report composition, persistence acknowledgement, job close and cancellation,
and the settings-transition fence are authority operations. Section 11 lists
them. An observation may be recorded after one of them succeeds, or, as the
seal of section 4.7, as the step that fixes what one of them composes over;
the observation is never how it happens.

### 4.11 The client stream

This is the server's contract for `/api/events`. How a browser adopts what it
receives is outside this brief.

1. **A revision and its event are queued in one step.** Every commit that
   produces a delta hands the delta to the broadcaster's mailbox before the
   state lock is released, as snapshots already are. `publish_delta_locked`
   requires that lock's state reference and accepts an owned `DeltaEvent`.
   The committing thread constructs the typed event and enqueues it; the
   broadcaster serialises it after releasing the mailbox mutex, without
   acquiring the state mutex. Queue order equals commit order for deltas and
   snapshots alike. Adjacent snapshots may coalesce, but never across a
   delta. Runtime cleanup and external dispatch remain outside the commit
   lock. A dispatch-failure observation made afterward takes its own revision
   instead of publishing with the earlier dispatch's revision. The client
   consumes a contiguous dispatch-failure revision from the current server;
   a real gap still requires an authoritative snapshot.
   The deterministic AppState tests suspend a text publisher immediately
   after its critical section, and suspend the real serializer while a
   concurrent commit completes. The measurement fixture compares mutex hold
   times with the base implementation on the same text chunk and records the
   worker's serialization time; timing values are observations, not pass
   thresholds. Test states constructed without a broadcaster retain their
   synchronous serialization fallback.
2. **Planned target: overflow is announced.** This remains a separate follow-up;
   today's bounded mailbox silently drops its oldest entry at capacity.
   In the target, when the mailbox is full, the broadcaster does
   not discard the oldest entry silently. It records that a range was lost
   and sends every client the marker it already sends a lagging client
   (`lagged`), followed by a snapshot at or after the lost range, built as
   `lagged_recovery_events` builds one today (`api_sse.rs:193`). The loss is
   counted and logged.
3. **The comment is corrected.** `publish_delta_locked`'s documentation states the
   contract of point 1.
4. **The watcher's consumers move off the per-event path.** File-change
   tracking and the carried fence read the watcher's batches, not each notify
   event under the state lock. This is safe only with the pending-batch
   admission and the maximum batch age of section 4.4; without them a
   received write could miss a checkpoint's cut.

Point 1 is implemented here. Point 2 remains a separate behaviour change
with its own tests (section 10), coordinated with the streamed-prefix fix
so that the server and client halves meet:

- The fix for that defect, committed and awaiting integration and not on
  `master` at `28e9ed5`, covers point 2 (a loss in the mailbox sets a flag that survives further
  drops and reaches every client as `lagged` with a current snapshot) and
  the client's repair of the visible transcript on that signal. It corrects
  the comment of point 3 without claiming the order is fixed. If it lands
  first, the step keeps what it landed.
- Point 1 queues retained deltas and snapshots in commit order. It does not
  establish the loss announcement and recovery guarantees of point 2.
- The client needs no per-session sequence from the server for its repair:
  the existing per-session mutation stamp and message count suffice.

## 5. The Engram host split

### 5.1 Components

Names are provisional. Each row is a module under one `engram_host` parent.

| Component | Owns | Built from |
| --- | --- | --- |
| `transport` | The control sidecar per session; the CLI runner and its lock retry; wire types and typed errors | From `engram_host_adapter.rs`: the wire types (`EngramControlRequest` through the receipts), `EngramTransportError`, the `EngramControlTransport` trait, the CLI runners (`run_engram_json_command`, `run_engram_cli_command` and their lock retry), `EngramProcessTree` and `EngramHostAdapter`. All of `engram_control_transport.rs` |
| `admission` | Bind, evaluate, begin; the pending dispatch; retained intents on queued prompts; work-binding reads; backoff and the circuit breaker | `prepare_engram_turn_delivery_off_lock`, `finish_engram_dispatch_record`, the bind and evaluate functions, `engram_queued_admission.rs`, `engram_work_binding_refresh.rs`, `engram_held_claims.rs` |
| `source_root` | Named roots: validation, the selection per work and store, generations, host lines about roots; the named-root binding to Engram (target, section 5.4) | `engram_source_roots.rs` |
| `verification` | Checks and carried checks; recognition, places and toolchain; the source basis at begin and close; the report, its cache and fallback; the checkpoint | `engram_turn_checks.rs`, `engram_check_*.rs`, `engram_write_places.rs`, `engram_one_call.rs`, `engram_carried_checks.rs`, `engram_turn_observations.rs`, `checkpoint_engram_turn_off_lock` and `finish_engram_checkpoint_record` |
| `recovery` | Boot recovery, uncertain grants, absence inspection | the boot-recovery functions, `engram_session_reconciliation.rs` |
| `settings` | Project and host settings, readiness and audit, the settings-transition fence | From `engram_host_adapter.rs`: `EngramProjectSettings`, the enablement and path validation, and the doctor and diagnostic runners (`run_engram_doctor_result_within`, `run_engram_diagnostic_within` and their output readers). `engram_readiness.rs`. The Engram parts of `session_crud.rs` |
| `context` | The Base tier: MCP composition, the orientation read | `engram_mcp_config.rs`, `codex_engram_bootstrap.rs` |

Acceptance evaluation (`acceptance_evaluation*.rs`) stays a separate area
that calls the facade. The test transports move to the component's tests.

### 5.2 The facade

`EngramHost` is what the rest of the host may call. It has three groups, and
nothing else in the component is visible.

- **Reports of facts.** A turn started; an execution transition; a workspace
  edit; a host write started or ended; a watcher batch; the test-run tick.
  In the first extraction these are direct calls that run today's code.
  Later they become recordings on the stream.
- **Authority operations.** Admit a turn; record its dispatch; close it with
  an outcome; name or clear a root; request an evaluation; apply a settings
  transition; recover after boot. Each returns a typed result.
- **Views.** What the wire projection and the UI need about a session
  (recovery pending, queue disposition, the control cards), about the host
  (which sessions have a revocation pending) and about a project (whether
  the operator disabled Engram), as an immutable snapshot; the root a turn
  is measured in; whether any check is open.

The facade names **reporting evidence** and **closing a grant** as two
operations. Both map to `turn_checkpoint` today, and Engram has no
operation that reports evidence without closing a grant. The separation
states intent; it does not mean a closed grant accepts another checkpoint,
and late evidence is never attributed to a new grant.

### 5.3 The contract with Engram

The contract is Engram's. Its canonical text is the section "Host integration
contract" of Engram's `docs/features/behavioral-control-plane.md`, with the
operations in its section "Host-facing control protocol". This brief reads it
at Engram commit `b5db8054f1c6a087ab007f6cf36e74f630efc2aa`, which carries
the amendment the two projects agreed for this brief: requirement 8 restated
for hosts that cannot intercept compaction, requirements 12 to 15, and the
paragraphs on what a checkpoint receipt proves, evidence order, dropped
evidence, evidence apart from a grant, and the replay of a named-root event.
A change to that text needs the concurrence of both projects' coordinators.
What follows is what TermAl calls and what TermAl guarantees on top.

**What TermAl calls.**

| Surface | Operations |
| --- | --- |
| Control sidecar, one process per session (`engram … control`) | `session_bind`, `session_status`, `turn_evaluate`, `turn_begin`, `turn_checkpoint`. `named_root_bind` exists in Engram and is not yet sent |
| Reads as the session | `work core held --json`; `work next --peek --context-generation termal-<n>` |
| Reads as the host reader | `work ls`, `work show` (windowed and full), `work memories`, `control-policy show` |
| Writes by command | `work evaluate` as the evaluator child; `control-policy set-acceptance-evaluation` on operator intent |
| Diagnostics | `readiness --json`, `doctor --json`, `control-session-inspect` |
| Agent-facing | the MCP child per session |

`authority revoke` is removed from this list: Engram has no such command.

**What TermAl guarantees on top.**

1. Admission order: bind, evaluate, begin, delivery, checkpoint. A retained
   bind or evaluate replays with the same payload and key.
2. One work binding per control session. Only a bound session reports
   evidence.
3. One report per grant. After an uncertain outcome (a timeout, a lost
   reply) the same report is resent under the same key. After a refusal the
   report changes, and its key changes with it, because the key hashes the
   report.
4. Inside a report, every verification names its producer by an observation
   id in the same checkpoint and its environment by its index in the same
   checkpoint (the contract also allows a stored id for either; TermAl sends
   neither). Every record in the report carries the one work binding. Ids
   are unique, non-empty and trimmed.
5. The fallback is TermAl's policy, not Engram's requirement: the report,
   then the turn's own observation, then nothing. Engram never asks for
   evidence to be dropped. Lost check evidence must be named to the holder.
6. Observation ids are unique within a turn by the check's position.
7. "Checkpointed" is never read as "tests recorded". A receipt proves the
   records whose ids it returns and no others, and those ids are compared
   with what was sent. A timeout or a lost reply is an unresolved write: it
   is settled by the replay of point 3 before any of that evidence counts as
   recorded. The receipt's cursors are audit positions and are compared with
   nothing.
8. An evaluator's brief and its evidence basis come from one read of the
   run's work view, begun after the receipt. The submitted basis is never
   raised while evidence from an older read is kept. Engram guarantees that
   every record a committed checkpoint returned is at or before such a later
   head, on three conditions the host keeps: the read is of the same run the
   checkpoint's binding names, not merely the same work item; it is begun
   after the receipt, never taken from a snapshot held from before; and the
   store's history is the continuing one, not an older file restored in its
   place. Inclusion follows from that order of calls, not from inspecting
   positions: the receipt returns ids without positions. Engram states its
   side of this guarantee in its `docs/features/acceptance-evaluation-lifecycle.md`,
   at Engram commit `e779a780df433998a138276d947c678f42dc4bd3`.

Points 3, 4 and 5 were first written differently and corrected by Engram's
maintainers against Engram's source: the first draft promised a verbatim
resend in all cases, a total order of evidence, a single fallback stage, one
root per report, and an id where a same-request environment takes an index.
Points 5 (naming lost evidence) and 7 (keeping the ids) describe the target;
section 2.7 describes today. Point 8 holds today for an evaluation requested
in a later turn, where the request's reads supply both; section 10.1 extends
it to a request made in the turn that ran the checks.

**Conformance.** The contract's requirements against TermAl at `28e9ed5`.
Rows 12 to 15 are the requirements the amendment adds. The contract states
what is right; this table states where TermAl stands and which step closes
each gap. The basis for rows 1 to 4, 7 and 10 is
[Engram host adapter](./engram-host-adapter.md); they were not re-derived
from source for this brief.

| # | Requirement | TermAl |
| --- | --- | --- |
| 1 | Declare mediated capabilities and assurance honestly | Binds with `turn_gated` and the effects the session's write policy allows |
| 2 | Bind a unique durable session and work run before task prompts | Binds under the TermAl session id; the work binding comes from `work core held`; a session may be bound without work |
| 3 | Evaluate each turn and surface a blocking directive | Yes, for projects with turn-gated control; the directive is a control card |
| 4 | Prevent prompts while the session is not ready | A prompt is delivered only after the matching begin receipt |
| 5, 6 | Action gating and action outcomes | Not built in Engram |
| 7 | Checkpoint before the next turn | Yes; a failed checkpoint leaves the grant open and the following bind closes it |
| 8 | Compaction (as restated in the amendment) | No runtime lets the host intercept compaction. TermAl learns of it from Codex and Claude after the fact and re-orients at the next prompt it dispatches, through one read that always passes `--peek` (`run_engram_context_nudge`, `engram_mcp_config.rs:429`). A turn the runtime continues by itself gets no host block first. For ACP runtimes TermAl never learns, so orientation is at session start only |
| 9 | Treat notifications as doorbells | TermAl subscribes to no Engram notification; every read is explicit |
| 10 | Resume through a fresh bind | Every restart rebinds; a begun grant is held for explicit resolution and never resent |
| 11 | Surface refusal codes and recovery actions | Codes appear on control cards. Gap: a fallback does not name the checks that lost evidence (step 8) |
| 12 | Resend the same report under the same key after an uncertain outcome; change the key when the report changes | Yes: the key hashes the report |
| 13 | Keep the receipt's record ids and compare them with the evidence sent; never present a closed grant as recorded evidence; name the evidence that was not recorded | Gap: the ids are discarded and lost evidence is not named (step 8). The receipt's cursors are compared with nothing: `EngramCheckpointReceipt` decodes them into `_cursor` and `_confirmed_cursor` (`engram_host_adapter.rs:799` and `:801`) and no code reads those fields. Two more decoded fields are likewise unread: the status response's `_confirmed_cursor` (`:700`) and the begin receipt's `_tentative_cursor` (`:785`). The only other matches of those names in `src/` are the test transports and fixtures that produce them. The `from_cursor`, `to_cursor` and `head_cursor` that the code does read (`:4593`) belong to a grant's delivery page, a different thing. Both receipt fields are required for the receipt to parse, which step 8 relaxes so that Engram can stop sending the second |
| 14 | Never open a grant only to carry evidence; never attribute a change made outside a turn to a later turn | Yes: a change made between two mediated turns is reported by neither |
| 15 | Bind a named root with `named_root_bind` before stating its generation; treat an unknown binding as no binding under that generation | TermAl sends no `named_root_bind` and states no generation, so Engram's named-root rules apply to none of its roots. The requirement's second half binds a host that attempted a bind; TermAl's roots are host state only and keep their current credit until step 7. The contract's minimum is to claim nothing under the unconfirmed generation; TermAl's design for step 7 is stricter and withholds the turn's source sighting, check credit and evaluation source until the binding is confirmed (section 5.4) |

**The evaluator's read, today.** A request takes `evidence_basis` from its
first windowed read of the work view and follows that read's continuation
token for older pages. Engram serves a continuation only while the project
has not moved since the first read and refuses it otherwise, so the pages of
one token are one read. When a continuation is refused or fails, TermAl
keeps the first read's basis and the pages it already has. That mixes no two
reads and raises no basis, so it is inside the contract, and it is a quality
gap: the evaluator is not shown the older evidence. The omission must be
counted and stated, in the brief and to the requester. That is a defect fix
of its own, which landed on `master` after this brief's base, as commit
`5d3e9cd`. At `28e9ed5` a request is not tied to any checkpoint, so
nothing compares the run in the view with a binding's run; the evaluation
hold adds that check and restarts the whole read, within its bound, when a
continuation is refused (section 10.1).

**Contract fields TermAl omits.** `named_root_bind`;
`source_root_generation` and `source_root_state` on a source basis;
`reported_source_change`; `check_kind: build`; a new measurement at
`done --source-fingerprint`. Each is a later step (section 10), owned as
section 5.4 and the verification component say.

### 5.4 The source-root component and the named-root binding

Work in progress for the named-root binding exists outside `master` (a
module of about 950 lines with its tests). Its design is the input for this
component, so that whoever completes it keeps the following.

- **Per work, not per session.** Rule 10 is kept inside the contract's one
  binding per control session (section 5.3, guarantees 2 and 4): every
  record of a checkpoint carries the grant's work, so a check is credited
  only to the work the turn's grant is bound to, measured in that work's
  named root, and credit for another held work never travels through this
  grant. No grant is opened, and no evidence is rebound, to carry it. What
  the rule forbids is substitution. A test the session runs in another
  held work's root is refused with a line naming that work and the focus
  change that binds the next turn to it; it is never credited to the bound
  work, and never silently dropped. An acceptance evaluation requested for
  a held work is resolved from that work's own claim and named root,
  whichever work the turn is bound to: the request is not checkpoint
  evidence, it opens the evaluator's read under the requested item's claim
  (guarantee 8), and only a request for the bound work in the turn that ran
  its checks passes through the evaluation hold (section 10.1). Work
  selection is explicit and precedes the turn: the session's focus chooses
  the claim the next bind takes (today the focused claim, else the current,
  else the newest, `select_engram_held_binding`), and a change of focus
  takes effect at the next bind, never inside a turn. The change from one
  resolution per session to this is a behaviour change with its own named
  proof (step 7 in section 10); the first extraction keeps today's
  resolution unchanged.
- **Scope and keys.** A selection is shared by (authority store, work,
  claim). It is not an evictable cache entry. The generation allocator and
  its high-water mark are host-wide and persisted, and generations seen from
  Engram raise the mark. The journal of intents is keyed by (store, claim).
- **The binding is an authority operation.** It stages an immutable intent
  (claim, fence, the workspace identity the source capture returns,
  generation, naming time, kind, end reason, the reporting session, a retry
  key from claim, generation and kind). The intent is acknowledged durable,
  by an exact fence on that claim's journal and selection, before the call is
  sent. After Engram's receipt, selection and receipt are acknowledged
  durable before the pending intent is cleared and success is announced.
- **Results.** Confirmed, refused, or unconfirmed. An unconfirmed binding is
  never reported to the agent as named and gives no source sighting, check
  credit or evaluation source.
- **Replay needs the reporter.** Engram's replay key includes the reporting
  session, and an exact replay needs that session's current connection. When
  the reporter is gone, recovery is the current holder's authoritative read
  of the claim it is bound to, stored as a reconciliation and never as a
  receipt. Engram would accept a matching read as settlement; TermAl stays
  more conservative and withholds named-root evidence until a fresh name.
- **Lifecycle.** A rename is one bind at a higher generation; there is no end
  for the old one. An end is valid only for the latest bound generation. A
  conflicting pending intent is never discarded locally: a new naming request
  is refused until replay or authoritative retirement settles it.
- **Reading state.** An absent `named_root` and an explicit `none` differ,
  and neither an absent nor an unknown state implies an end. Absence from one
  session's held list proves nothing about a shared binding.
- **Provenance.** A turn keeps the generation and workspace it was admitted
  with through a rename or clear made during it. A sighting states generation
  and state only for a confirmed event.
- **Seam.** The source-root owner holds the selection, allocator, journal and
  reconciliation, and hands out immutable snapshots. Verification owns
  content capture, check evidence and credit. Admission owns the root
  snapshot a grant was admitted with. The journal's compaction stops scanning
  other components' state; they hold explicit retention pins instead.
- **Planned on Engram's side.** A bounded, read-only, claim-scoped read of a
  claim's root lifecycle. Without it a host cannot learn the fate of a root
  whose callers have all moved to other claims, and TermAl's limit of 64
  selections could refuse new names for good. It is planned, not available.

### 5.5 Order of moves

Each is a pure move unless marked, and each lands with the full gate and a
review pair.

1. Test transports out of the production file, into the tests.
2. `transport`: wire types, errors, the CLI runner, the sidecar.
3. `recovery`.
4. `settings`, without the dead authority-retirement path, whose removal is
   its own change.
5. `context`.
6. `source_root`, as the base the named-root binding is built on.
7. `admission`.
8. `verification`, last, after the first extraction has given it a facade
   (section 9).

`transport` goes first because everything else uses it and it uses nothing
else. `verification` goes last because it is where the behaviour is densest
and where the first extraction already draws the boundary.

## 6. Owned state

### 6.1 Two ways to own

- **Embedded.** The state stays where it is, on `SessionRecord` or
  `StateInner`, under the state lock and persisted as now, but its type
  belongs to a component and its fields are private. Other code holds the
  record and calls methods on the component's type. No lock changes.
- **Separate.** The component keeps its own map and its own lock.

Embedded is the default: it enforces ownership without changing a lock or a
persistence path, so a step that adopts it can be a pure move plus
visibility. Separate is chosen only with a stated reason. The execution owner
(section 8) and the event log are separate, because they are written from
threads that must not take the state lock.

### 6.2 `EngramSessionState`, field by field

This table is target ownership. It says who should own each field, not what
moves when. State whose lock or lifecycle cannot be handed over as a pure
move stays with its embedded owner under the state lock until its handoff is
specified and tested as a step. At no point are two copies of a fact both
authoritative.

| Owner | Fields |
| --- | --- |
| `admission` | `admission_in_progress`, `recovered_admission`, `routing_token`, `work_binding`, `refused_work_bindings`, `work_binding_refresh_rebinds`, `turn_begun_since_binding_read`, `active_grant_id`, `active_turn_intent_fingerprint`, `active_turn_grant_mutates`, `uncertain_grant_id`, `begins_recorded`, `dispatch_generation`, `bind_in_progress`, `pending_dispatch`, `rebind_required`, `disabled_reason` |
| `transport` | `consecutive_transport_failures`, `circuit_open`, `next_bind_retry_at` |
| `verification` | `active_turn_checks`, `carried_checks`, `carried_consumed_runs`, `carried_unmatched_launches`, `capture_workers`, `withheld_command_keys`, `next_turn_check_sequence`, `active_turn_start_basis`, `active_turn_report`, `active_turn_report_fallback`, `checkpoint_in_progress`, `checkpoint_owner_generation` |
| execution owner (section 8) | `running_command_keys`, `running_command_worktrees`, `workdir_worktree`, `shell_directory` |
| `source_root` | `active_turn_source_root`; and, as the queue of host lines for the agent, `pending_source_root_line`, `source_root_line_delivery` |
| `settings` | `project_reset_in_progress` |
| `context` | `context_nudge_pending`, `context_nudge_in_progress`, `context_nudge_in_progress_generation`, `context_nudge_generation`, `pending_context_nudge`, `context_nudge_delivery_generation`, `context_nudge_delivery_turn_generation`, `context_refresh_needed`, `signalled_compaction_item_ids` |

Two notes. The four fields that say where a session's commands run are facts
about executions, not about checks; this brief reads the first defect of
section 2.9 as a consequence of treating them as check bookkeeping. They stay
embedded, under the state lock and authoritative, through the first
extraction; the execution owner's copy is a mirror until a later step hands
them over. And `pending_source_root_line` has
become the queue for every host line to the agent, check-credit lines
included; it should be named for what it is when its owner is extracted.

### 6.3 Other Engram state

| Where | Fields | Owner |
| --- | --- | --- |
| `SessionRecord` | `engram_mcp_installed`, `engram_mcp_runtime_quarantined`, `engram_mcp_revocation_pending` | `context` |
| `SessionRecord` | `engram_boot_recovery_pending`, `engram_boot_recovery_dispatch_pending`, `engram_boot_recovery_retry_in_progress` | `recovery` |
| `StateInner` | `engram_host_adapter`, `test_engram_dispatch_budget` (tests only) | `transport`, `admission` |
| `StateInner` | `engram_declared_project_ids`, `engram_declaration_checked_project_ids`, `engram_project_resets`, `engram_retired_work_authority_grants` (dead) | `settings` |
| `StateInner` | `engram_work_source_roots`, `engram_source_root_generation`, `engram_source_root_validations_live` | `source_root` |
| `StateInner` | `engram_turn_basis_captures_live` | `verification` |
| `AppState` | `engram_carried_poll_lock` | `verification` |
| `QueuedPromptRecord` (`session_interaction.rs:163` to `:169`) | `engram_waiting`, `engram_bind`, `engram_evaluate`, `engram_interrupted`: the retained intents and the interrupted marker of a queued prompt | `admission` |
| Derived at serialisation | `pending_engram_mcp_revocation_session_ids` on the host-level state response (`wire.rs:2502`); `engram_operator_disabled` per project (`wire.rs:358`) | `context`; `settings` |

### 6.4 What is persisted and what the client sees

Persisted per session today: the routing token, the open grant, the
uncertain grant, whether begins were recorded, the dispatch generation, a
marker per carried launch, and, with the session's queue
(`persisted_state.rs:331`), the retained intents and interrupted marker of
each queued prompt. Persisted per host: the named roots and their generation
counter, and the dead ledger. Everything else is per process.

Per session, a client sees one Engram field, `engramBootRecoveryPending`,
the queue's retained and interrupted dispositions, and the control cards in
the transcript. Per host it sees the sessions whose MCP revocation is
pending. Per project it sees the settings, whether the repository declares
Engram, whether the operator disabled it, and a cleanup warning. The views
of section 5.2 are exactly these.

### 6.5 Locks

| Lock | Guards | May be held while taking |
| --- | --- | --- |
| State lock (`AppState.inner`) | `StateInner`, every `SessionRecord`, all embedded component state | a capture's result mutex, briefly, to read readiness (as today: `open_to_writes` reads it); the broadcaster mailbox's mutex (as today for snapshots, and for deltas from step 4); the event log's lock |
| Broadcaster mailbox's mutex (`StateBroadcastMailbox.pending`, `state.rs:413`) | the pending snapshots and deltas of the client stream | nothing; it is a leaf |
| Execution owner's lock | execution records and the running set | the event log's lock |
| Supervisor's state lock | process identities, resources and the admission of a cancellation | the event log's lock; released before any process operation, wait, drain or storage |
| A capture's result mutex | one snapshot's value and readiness | the event log's lock |
| Carried-poll lock | one poll of carried runs at a time | the state lock, as today |
| Launch guard | the inheritable-handle window of one launch | nothing; no recording while held |
| Event log's lock | the log | nothing |

The longest chain kept from today is: carried-poll lock, state lock, a
capture's result mutex, taken there only to read readiness. The event log's
lock is new; it is a leaf at the end of that chain and of every other, and
under a capture's mutex it is taken by the capture's own thread, which
holds no state lock. Never the reverse: no capture
thread takes the state lock while holding its mutex, nothing takes the
carried-poll lock while holding the state lock, and nothing waits or does
I/O under the state lock. These retained nestings are revisited only by an
explicit lock migration.

The carried-poll lock is taken by the checkpoint and by the test-run tick.
The checkpoint's wait on it has no bound today; that is tracked (section 13)
and must be bounded before the verification move.

## 7. Process launch and supervision

### 7.1 Ownership and scope

**Current at 28e9ed5.** `src/host_command.rs` serializes the Windows
inheritable-handle window for standard host launches and the native primitive.
`src/windows_launch.rs::prepare` provides native containment for terminal and
bounded-read commands. Agent runtimes and the Engram adapter have not migrated
to that native primitive. The shared creation guard is not itself job
supervision.

**Target.** One private process-supervision component owns host process
creation and lifetime. Its handle admits an explicit launch specification and
lifetime policy, accepts generation-fenced cancellation, and returns immutable
identity and result receipts. Platform backends retain the existing Windows
and Unix launch semantics. Protocol adapters continue to own command
construction, provider protocols and response interpretation; `EngramHost`
owns admission and credit authority. Neither becomes a second process owner.

| Subject | Owner and boundary |
| --- | --- |
| Host-created process | Supervisor owns original process identity, launch resources, cancellation, waits and cleanup. A PID is diagnostic metadata, never sufficient cleanup authority. |
| Shared Codex app-server | One supervised process belongs to a runtime generation and serves many sessions and turns. Ending one turn does not terminate that process or complete unrelated executions. Runtime shutdown remains a separately fenced operation. |
| Provider-reported command | Execution component records observations; TermAl does not acquire OS ownership merely because a provider reports a command. |
| Launcher run | A separately identified logical run, linked to its launch and observed stages. Its persisted result does not establish descendant termination. |

The recommended boundary is a real Rust module with private resources and one
handle. An `include!()` fragment with broadly visible fields is the
alternative: it makes the initial move smaller, but cannot prevent callers
from taking a job lease, bypassing cancellation fencing or mutating execution
state. Transitional fragment bridges must delegate to the private owner and
have named removal steps.

### 7.2 Creation, supervision and locks

Preserve the landed Windows sequence: build the supported `LaunchSpec` at the
call site; retain original process and primary-thread handles; attach pipe
readers before one resume; keep the sole non-inherited job lease in the
supervisor, separate from cloned waiter handles. Native stdio originals are
non-inheritable. Temporary inheritable duplicates are created and closed
inside the same `host_command::inheritance_lock` window used by standard
launches. Atomic job-assignment refusal uses a fresh job for suspended
assignment.

The creation guard ends after creation and disposal of temporary inheritable
handles. It covers no wait, drain, persistence, event delivery or production
observer callback. Controlled test-only creation hooks do not transfer
ownership or introduce production callback dependencies. The caller releases
`StateInner` before launch. Supervisor state is claimed briefly to validate a
generation-fenced action and retain the original resource identity needed to
perform it. Process operations and waits happen outside that state lock. The
action uses those retained resources; it never looks up a replacement by
session name, current generation or PID after unlocking. Job close, resume and
cancellation are direct owner operations, never work delegated to event
subscribers.

Cleanup has no dependency on subscriber progress, persistence acknowledgement
or successful event publication. An observer receives no process/thread/job
handle and cannot extend lease lifetime. Waiter handles remain internal. A
blocked observer cannot prevent root-exit handling, job close or cancellation.
Root exit, cleanup requested, cleanup observed, pipe EOF and output-drain
result are separate facts: initiating job teardown does not prove every
process has exited.

The target removes mutable process resources from the cloned runtime handles
in `src/session_runtime.rs`; protocol writers retain only the input channels
they require and an opaque supervisor identity. Stop requests name that
identity and runtime generation. A stale request cannot stop a replacement
runtime.

### 7.3 Explicit lifetime and I/O policies

| Policy | Preserved behavior |
| --- | --- |
| Bounded read | Caller supplies a deadline and stdout bound. Preserve stderr draining/truncation and the shared bounded drain allowance; deadline expiry and output failure trigger supervisor cleanup. Preserve the difference between unavailable containment and a failed launch. |
| Terminal | Foreground commands may be long-lived: no universal production deadline is added. Preserve Stop/disconnect cancellation, output caps and stream behavior. The stream can request cancellation through the supervisor; it never owns the lease. |
| Agent runtime / adapter | Preserve protocol-specific input, readiness, request deadlines and shutdown behavior. A request timeout or session detachment is not automatically process death. Shared runtime ownership stays above individual session/turn ownership. |

These are policies over common primitives, not one timeout or pipe strategy
applied everywhere. A pure extraction preserves the behavior in
`src/bounded_read_process.rs::run_bounded_read_windows_with_setup`,
`src/terminal.rs::run_terminal_shell_command_with_timeout_and_stream` and the
runtime adapters.

The landed native specification supports ordinary argv, inherited environment
with edits/removals, cwd and the flags its callers use. It does not
reconstruct opaque `Command` state or support `raw_arg`, `env_clear` or direct
batch-shim execution. New support is a separate behavior change. Containment
can be unavailable while the command still runs; packaged identity may change;
later Store-packaged descendants can escape; suspended fallback has a crash
window before assignment. Preserve these limits from [Windows command
containment](windows-command-containment.md), including Unix's existing
process-group behavior.

### 7.4 Evidence and proof required for migration

Existing coverage includes `src/tests/windows_launch.rs` for retained
identities, root-versus-intermediate exit, fresh-job fallback,
unrelated-handle exclusion, argv/environment/cwd parity and real-launcher
recovery. In particular,
`host_windows_command_capture_releases_the_lock_before_waiting` constrains the
guard lifetime. `src/tests/terminal.rs`, `bounded_read_process.rs`,
`shared_codex.rs` and `shared_codex_events.rs` constrain their respective
policies. Some old Windows bounded-read/freeze tests use the test-only legacy
transport; they are not proof of native production deadline/output-limit
behavior. That native coverage gap must remain explicit until its dedicated
regressions land.

The existing intermediate-exit test also has a diagnosed fixture assertion
defect: `native_windows_launch_intermediate_exit_does_not_close_root_lease`
checks that the leaf is still alive immediately after terminating its parent,
but the leaf can exit before that check. The diagnostic retained a live root
while the leaf exited. Until corrected, a passing run of this test is not
stable proof of leaf survival after an intermediate exit. This does not
establish a product lease defect; retain the root-lifetime requirement and
prove any required leaf-survival behavior with a suitable fixture and
readiness barriers.

The supervisor migration needs a new end-to-end proof with readiness barriers
and retained identities: a host-owned root launches descendants, a subscriber
is deliberately blocked, and root exit or cancellation still closes the sole
lease and observes the owned members' exit. The subscriber later receives the
same typed result without owning resources. Add stale-generation cancellation
and shared-Codex isolation cases: stopping one turn leaves the server and
another session's execution alive. Exercise bounded-read deadline/output-limit
and terminal cancellation through the actual production backend. A missing
Store package is an explicit not-applicable result, not a containment pass.
These are required future proofs, not tests run for this brief.

## 8. Execution record

### 8.1 Identity, origin and frozen context

**Target.** A private execution component owns execution identity,
correlation, transitions and recovery. One record shape has three explicit
origins: host process, provider command and launcher run. Links between them
express launch or participation, never shared lifetime. One shared runtime
process can serve many command executions; a detached launch can finish while
its linked launcher run remains unresolved.

The component allocates immutable host execution ids through its handle. A
lock-free allocation reserves identity only; it establishes neither
registration nor durability. Every ingress has an `AdmissionId`. Its
`ExecutionId` may be absent until correlation resolves; a first start may
carry a reserved id. `StateInner` keeps no duplicate execution-correlation
map.

Provider correlation includes provider/runtime generation, session/thread,
turn, call/item and attempt or occurrence identity as available. Repeated
descriptions and starts of the same open occurrence reuse its id. A genuinely
new invocation reusing a finished key gets another id. Completion-only
observations can create a record without inventing a start or source capture.
Ambiguous late responses remain unattributed rather than attaching to the
newest occurrence. PID and command text alone are insufficient identity.

Keep the context observed for that execution: cwd and its provenance,
execution/shell scope, command identity, runtime and turn generations, and
immutable attribution supplied by `EngramHost`. For a mediated check that
attribution retains the original store/work/run/claim/grant, named root and
generation, toolchain/environment observations and start/end source captures.
Missing values and unavailable captures are typed states, not values
reconstructed from the session's current settings. Engram serialization stays
in the adapter; the record carries enough capture provenance for root
generation and state without inventing support in the current wire type.

A command's directory and a persistent shell's directory are separate facts.
Background/private-shell execution must not mutate persistent-shell location
merely because its launch line contains `pushd`. Conversely, a supported
foreground shell move retains its defined effect. Cwd supplied by the
provider, inferred shell position and unresolved write places remain
distinguishable. Later descriptions add attributed observations; they do not
rewrite what the host knew at start.

### 8.2 Lifecycle, terminal result and observation continuity

Lifecycle and observation continuity are independent. Pending and Started
describe executions without an authoritative terminal witness. Continuity may
be live, lost or reconciling while either remains unresolved.

| Observation or result | Meaning |
| --- | --- |
| Started / described | An origin-specific start or description; a provider report is not a host-observed OS spawn. |
| Launch accepted / not finished | Progress for continuing work. A wrapper's successful detached-launch result does not complete the linked run. |
| Completed | An authoritative terminal observation for this subject, retaining typed code/signal/provider status or explicitly unavailable exit detail and its provenance. Unavailable detail is not success. |
| Launch failed | Positive evidence this attempt did not launch; uncertainty after a launch request is not a failed-spawn witness. |
| Interrupted | Terminal only with a named witness authoritative for this subject. A cancellation request, lost response, closed observer or host restart alone is insufficient. |
| Continuity lost / reconciling | Idempotent annotation of an observation gap under the original execution id. It permits later reconciliation and claims no process death. |

There is at most one authoritative terminal transition per execution. Its
identity is `(execution_id, transition_version)`; duplicate delivery or replay
does not create another terminal fact. Conflicting reports are retained as
reconciliation diagnostics, not a terminal overwrite. Later authoritative
completion of an unresolved execution uses the original id.

Terminal status does not imply complete output or successful cleanup. Root
exit, descendant observations, containment, output truncation and drain errors
have separate typed observations. Evidence may arrive later without mutating
the immutable terminal result. A process may have exited zero while its check
remains ineligible.

### 8.3 Recording, cuts and durability

Admission is recorded synchronously with the generation snapshot; off-lock
resolution references that `AdmissionId`. The execution owner commits a
transition and appends its immutable receipt under its short lock followed by
the stream's leaf append lock. No I/O or callback occurs there. No
`StateInner`, execution-state or stream lock spans acquisition of the launch
guard or process creation. Pre-launch admission is released before creation;
creation observations are appended after the launch guard ends. Dispatch
occurs outside those locks.

A complete cut requires both a sealed admission point and the judging
consumer's installed resolution watermark. Admissions through the cut that
have not resolved are explicit uncertainty, conservatively affecting every
worktree when their place is unknown. They cannot disappear through eviction
or generation rejection. A stale generation prevents misattribution of credit
but does not erase interference with other executions. Bounded logs retain
compact uncertainty/active-writer summaries rather than pinning unlimited
event entries. The stream's epoch-local sequence orders delivery; it is not
durable execution identity.

There are two receipts: `Recorded(version)` and `Durable(version)`. The former
acknowledges an in-memory transition. The latter acknowledges persistence of
that exact version and its referenced identity/context. Persistence consumes
immutable versioned payloads; acknowledgements return to the owning component
outside `StateInner` and execution locks. Prefer the existing persistence
writer with execution-owned records and bounded enumeration; it may not fetch
mutable execution state while holding `StateInner`. The durable implementation
must include referenced attribution/capture state or an equally durable
reference, so its receipt cannot point to missing context.

Extraction 1 deliberately stops at the structural boundary. It preserves the
existing direct check-evidence path and adds no turn-close persistence wait.
Provisional execution records provide no new authority or credit and no
restart-recoverability promise. The immediately following durability step
establishes durable pending/start identity and terminal acknowledgement before
credit depends on the execution record. Slow or failed persistence then yields
bounded waiting and explicit withheld credit, never a fabricated success.
Process cleanup never waits for either receipt.

Recovery enumerates retained durable records with bounded pages. A durable
terminal fact remains queryable by id/version; the process-local event stream
need not replay globally across restart. An unresolved record receives a
continuity annotation, once per loss, and retains its original identity and
context for reconciliation. Records that never reached the durable boundary
are outside the recoverability claim. Execution reconciliation does not revive
restart-lost check credit.

### 8.4 Launcher recovery is a declaration about a run

The recover utility invocation, `LauncherRun` and observed stage/process
executions are distinct linked records. In
`scripts/test-launcher.mjs::recoverRun`, successful persistence of a validated
settlement after the recorded worker is judged gone is an explicit witness for
the `LauncherRun`'s terminal interruption. Calling `recover`, seeing its
command succeed or restarting the host is not that witness.

The current saved result has `state=failed`, `exitCode=1`, `interrupted=true`
and an end time. That exit code is a declared run failure, not an observed
stage exit. A previously running stage is marked failed with an explicit
unknown outcome. Recovery neither reruns it nor proves its exit, descendant
cleanup or pipe EOF. Linked processes lacking their own terminal witness
remain unresolved and can complete later under their original ids. Their
completion cannot change the interrupted run into a pass.

Recovery rereads under its lock and preserves an existing terminal result,
including one the worker saved after the first read. A failed settlement save
supplies no new terminal witness. Failure to notify after a successful save
does not erase the saved fact. Repeating recovery observes the same
settlement. A later conflicting run artifact is a conflict, not a second
terminal transition.

These claims are pinned by `scripts/test-launcher.mjs::recoverRun`,
`scripts/test-launcher.test.mjs` tests “recover settles a dead worker's run as
interrupted, and only the owner notifies, once”, “recover keeps a terminal
result the worker saved after recovery first read the run”, and the
failed-write/notification tests.
`src/engram_carried_checks.rs::engram_read_carried_run` independently refuses
`interrupted=true`: the recovered run supplies neither passed-test nor
ordinary failed-test evidence. See [the test launcher guide](../test.md).

### 8.5 Execution outcome and credited check are separate records

The verification owner projects a check from execution observations plus
recognition, source, environment, authority and interference evidence. It owns
carry/drop policy, unmatched-launch ambiguity, artifact attribution and
expiry. The generic execution component owns none of those Engram decisions.

Preserve the current fences in `src/engram_turn_checks.rs`:
`EngramTurnCheck::open_to_writes` stays true until BOTH start and end captures
are ready; pre-existing writable sessions, other running commands, late naming
and unknown worktrees matter; `engram_merge_live_overlaps` reconciles marks
received during off-lock capture waits.
`src/engram_turn_observations.rs::engram_turn_report_plan` preserves the
report for its original grant across retries. Carried checks additionally pin
the first terminal artifact and validate the live generation/fence/conflict
when taking checks for a checkpoint. Interval reconstruction is later work
with its own equivalence proof.

The target credit decision records structured reasons and witnesses:
recognition or binding failure, missing/changed basis, unavailable
environment, absence of positive test evidence, overlap, gap, interrupted run
or uncertain report delivery. An interference reason distinguishes a
positively observed write from conservative possible-writer activity, an
observation gap or an unresolved place. It retains the policy witness actually
used, naming the session/execution, place and phase when available, or
explicitly stating which were unknown. It never upgrades peer activity into
proof of a file write or retroactively guesses a writer. Diagnostic summaries
agree with the typed outcome. PASS prose alone is neither process authority
nor sufficient test evidence.

A known execution result remains known even when its check is withheld or
reported unknown. This distinction addresses the foreground-gate case where
exit zero and passing output survived but the credit record was indeterminate,
and the background-shell case where a later command was attributed to the
wrong worktree. Those reports motivate the model; this brief does not
establish their exact causes or implement their separate fixes. A continuation
wait is not another execution merely because it is another API call.

After durable-record-backed credit is enabled, the credited-check projection
links the original execution/check and report attempt to Engram's returned
stored verification id, scoped to the original store/work/run/grant. The
linkage does not change the execution's terminal result. A grant-close receipt
without that check's stored id is not credit. One returned id witnesses a
credit; uniqueness also requires stable logical credit identity, exact replay
of uncertain attempts and reconciliation if the host crashes before saving the
linkage. Definite refusal may produce a changed report and corresponding key;
an uncertain outcome cannot.

Current checkpoint reporting and grant closure remain coupled on the Engram
wire. Separate facade intents do not invent a report-only operation or allow
evidence to be rebound to a new grant. Late evidence independent of a closed
grant remains a joint-contract capability until explicitly delivered. See
[Engram host adapter](engram-host-adapter.md).

### 8.6 Proof required for the execution component

Existing constraints come from `src/tests/engram_turn_checks.rs`,
`engram_one_call_checks.rs`, `engram_turn_observations.rs`,
`engram_carried_checks.rs`, shared-Codex tests, and
`scripts/test-launcher.test.mjs`. They are separate from native
process-containment proof.

Add deterministic end-to-end cases for duplicate/reordered starts and
completions, key reuse, completion without start, continuation waits, and one
runtime serving concurrent turns. During extraction 1 these establish
projection identity and preserve the existing direct credit path. In the later
step that makes complete cuts authoritative, exercise a delayed admission
resolution across checkpoint close: credit is withheld until the resolution is
installed or uncertainty is recorded. Exercise missing/reordered capture
readiness, unknown places, gaps and a write during final reconciliation
without weakening today's fences.

The durability step needs crash injection before/after pending persistence,
terminal persistence, Engram checkpoint commitment and local credit-link
persistence. Recovery must neither invent a result nor duplicate a credit.
Demonstrate both a clean execution receiving credit and a completed execution
withheld with a precise reason; safety-only tests that withhold everything do
not prove the design. Add a recovered interrupted launcher whose stage later
exits successfully: execution reconciliation succeeds, the run remains
interrupted, and credit remains withheld. Background-shell isolation and
native bounded-read coverage are explicit companion regressions, not claims
that this brief has fixed those gaps.

## 9. The first extraction

Command completion, to execution record, to credited check, through the
`EngramHost` facade.

### 9.1 What it is

A change of ownership and types. It moves no rule about credit and adds
none. The overlap marking, the snapshot captures, the carried-check fence and
the checkpoint's composition run exactly as they do at the extraction's base
commit, inside the component, under the locks they hold today, and they
remain the only authority for checks and credit. The execution owner's
records and the stream's observations are provisional mirrors of the same
facts. Nothing that decides credit reads them in this step.

### 9.2 Before and after

Before: the recorder calls five functions of the Engram host directly, at
seven places (`recorders.rs:758`, `:808`, `:837`, `:1135`, `:1162`, `:1261`,
`:1288`). The command handlers receive a session id, the runtime's command
key and strings, and look up the remaining context under the state lock; the
workspace-edit handler receives the session id.

After:

1. The recorder reports each provider event as a typed observation. For a
   command event it first records the admission, straight on the log, in the
   critical section in which it reads the session's generations under the
   state lock. After releasing that lock it hands the event to the execution
   owner, which records the transition once under its own lock (its commit
   and its append to the stream are one step, section 8.3) and returns the
   immutable receipt: execution id, transition version, origin, the
   admission it resolves, the generations it was admitted under, and a typed
   payload. The state lock is never held while the execution owner's lock is
   taken. A workspace edit has no command key and no execution. It is an
   observation of its own, and no execution is invented for it.
2. The recorder passes the typed observation to `EngramHost`: an execution
   observation carries its transition receipt, a workspace edit carries its
   session. One facade operation with a typed input replaces the five
   functions. The facade does not append the transition a second time.
3. `EngramHost` invokes the existing handlers at their existing lifecycle
   point, with their existing arguments, lock and generation checks and
   snapshot timing. The start and end captures are taken when they are taken
   today and are never postponed until a stream resolution. The provisional
   execution metadata mirrors these facts; it is not a new source of
   authority. A later step makes the resolved identity and context
   authoritative, with its own tests.
4. No stream consumer changes check state in this step.

The host-write calls in `terminal.rs`, `api_git.rs`, `api_files.rs` and
`api_review.rs`, the turn-start calls, the watcher call and the test-run
tick's call become distinct facade operations for host facts in the same
step, with their bodies unchanged. After it, no file outside the component
names a function of the check machinery.

### 9.3 What it does not do

- It does not replace overlap marking by intervals (section 10, step 10a).
- It does not make execution records durable. They are provisional, and
  nothing new depends on them for authority or credit. The invariant "a
  credit cites a durable record of the completion it rests on" is a target
  this step leaves unmet; step 2 of section 10 establishes it.
- It does not move the state that says where a session's commands run. The
  running sets and the shell's position stay embedded, under the state lock
  and authoritative; the execution owner's copy is a mirror.
- It lets neither a cut (section 4.7) nor an ambiguous correlation decide
  credit. The checkpoint composes its report as it does today. A rule by
  which an incomplete cut or an ambiguous completion withholds a credit is
  a behaviour change and lands in a step that names it (section 10, step
  10b).
- It sends Engram no new reference. The observation and producer ids in a
  report and the replay keys are byte for byte what they are today.
- It does not retain receipt ids or name lost evidence. That is a defect fix
  of its own; if it lands first, this step preserves it.

### 9.4 Tests that must stay green, unchanged

`src/tests/engram_turn_checks.rs`, `engram_one_call_checks.rs`,
`engram_lost_shell_overlap.rs`, `engram_carried_checks.rs`,
`engram_carried_gate_live.rs`, `engram_turn_observations.rs` and
`engram_host_adapter.rs`. "Unchanged" means their assertions; where a test
calls one of the five functions directly, the call may change to the facade
operation and nothing else.

### 9.5 New proofs

1. **End to end.** A provider command that is a recognised test is driven
   through the recorder. The test asserts the execution record, then the
   checkpoint request. The request's observations, evidence and idempotency
   key equal those of a fixture captured from the extraction's exact base
   commit for the same input. Four values are each checked on their own and
   compared as placeholders: the worktree root the run happens to use; the
   environment fingerprint, which hashes that root; the stamped times, by the
   record that carries each; and the content revision, which is the same on
   every run on one machine but has not been shown equal on every platform.
   The base has no clock to inject, and adding one would have changed
   production code before the capture. The base is `28e9ed5` unless a correctness fix lands
   first; such a fix is part of the base and is preserved, never compared
   away. Fixes did land first: the extraction began at `cac212c`, and the
   fixture test records that commit as the base it was captured at. The test
   gains the execution record's assertion in the commit that introduces the
   record.
2. **The ingress seal, on the projection.** A recorder is paused between
   Admitted and Resolved. The observation projection's cut through a later
   position is `Incomplete` and names that admission. The same test asserts
   that the checkpoint's report is what it was before this step: the cut
   decides nothing yet.
3. **A stuck consumer.** A consumer that never returns delays neither a
   recording, nor a checkpoint past its existing budget, nor process cleanup.
4. **One terminal transition.** A completion delivered twice yields one
   terminal transition for its execution and no additional credit from the
   existing path. A new invocation that reuses a finished key is a different
   execution with a different id, and may earn its own check.
5. **The boundary.** With option 1 of section 3.1, the component's fields the
   step made private are not reachable from outside it; the build is the
   proof.

**The first consumer.** The evaluation hold is the stream's first real
consumer. It is step 3, not part of this extraction, and section 10.1
specifies it.

## 10. Later steps

In order. Each names the tests that constrain it and the proof it adds.
Defect fixes that are independent of the structure (section 13) may land at
any point.

| # | Step | Kind | Constrained by | New proof |
| --- | --- | --- | --- | --- |
| 1 | The first extraction (section 9) | ownership, types | section 9.4 | section 9.5 |
| 2 | Durable execution records and record-backed credit (section 8) | behaviour | `src/tests/engram_turn_checks.rs`, `engram_turn_observations.rs`, `engram_carried_checks.rs`; persistence and checkpoint replay tests; `scripts/test-launcher.test.mjs` | Inject crashes before/after pending identity, terminal persistence, Engram checkpoint commit and local credit-link persistence. A non-durable completion never becomes recovered success; a clean durable completion earns one credit, retry/recovery cannot duplicate it, and slow/failed persistence withholds with a reason. A recovered interrupted launcher whose stage later exits zero remains interrupted and uncredited. |
| 3 | The evaluation hold as the stream's first consumer (section 10.1) | behaviour (a required fix) | acceptance-evaluation tests | End to end: the requesting tool call returns before its turn ends, and the evaluator reads evidence only after the matching checkpoint receipt. An evaluation requested in the turn that ran a passing bound test passes; a request in a turn without checks starts at once; a lost wake, an unknown checkpoint outcome and a restart each end in a stated refusal within the bound, never in a stranded request |
| 4 | The client stream contract (section 4.11): point 1, and points 2 and 3 where the streamed-prefix fix has not already landed them | behaviour | the mailbox tests in `state.rs`, which pin today's silent drop and change with point 2; the SSE tests | Two threads committing concurrently are received in revision order; 257 queued deltas with a stalled broadcaster produce the marker and a snapshot, never a silent gap; the state lock's hold time is measured |
| 5 | The adapter moves of section 5.5 | pure moves | the whole gate, unchanged | The boundary, per move |
| 6 | Private fields, component by component (section 6) | ownership | the whole gate | The boundary; the count of outside field accesses reaches zero for the component |
| 7 | The source-root component and the named-root binding (section 5.4) | behaviour | the existing source-root tests; the work in progress's tests | Bind, rename, clear and restart recovery against a real Engram binary; an unconfirmed binding credits nothing; a session holding two claims, bound to the first: a test run in the second claim's root is refused with a line naming that claim and the focus change, and is not credited to the first; after the focus change the next turn binds to the second claim and the same test is credited to it, in its root; an evaluation requested for either claim, in a turn bound to the other, names the requested item's claim and root |
| 8 | Contract fields: receipt ids kept and compared, `reported_source_change`, `check_kind: build` | behaviour | report-layout tests | A partial store is detected and named to the holder |
| 9 | Execution context handed to the execution owner: running commands, their places, the shell's position (sections 6.2 and 8.1) | behaviour-preserving handoff | `src/tests/engram_turn_checks.rs`, `engram_one_call_checks.rs`, `engram_lost_shell_overlap.rs`, unchanged | One authoritative copy at every point of the handoff. A command's directory and the persistent shell's directory are recorded as separate facts; the fix for the false refusal after a background launch has either landed before or is a named behaviour change of this step |
| 10a | Overlap from intervals: the marking is replaced by a projection over the stream, with the same outcomes | behaviour-preserving replacement | every overlap test, unchanged | Equivalence with today's rules on recorded histories, and the race tests below |
| 10b | The drain, the seal and a complete cut through the seal (section 4.7) are required before the checkpoint composes its report | behaviour: an incomplete cut withholds | the tests of 10a | A delayed admission resolution across the checkpoint's close withholds the credit until it is installed or recorded as uncertain; a resolved, clean check is still credited; a deterministic test pauses the checkpoint after its cut is `Complete` and before the report is frozen, admits a watcher batch in that interval, and requires its position after the seal and the report unchanged, while a batch delivered to the watcher's channel before the drain marker is admitted before the seal and, unresolved, withholds; the window test: A has one capture pending at seal `S` (end ready before start ready, and the reverse); B is admitted after `S` while that capture is pending; A's captures then complete. A is withheld from the composition through `S`. B remains recorded as interference with A for any later composition permitted by A's original-grant and carry rules; closing that grant does not move A's evidence to a new grant; the positive case: A's later readiness before the seal, B a genuinely later write admitted after it: A is credited and B fences nothing of A's; the same two histories for a carried run with its terminal read pinned before and after the seal |
| 11 | One process supervisor for every host launch (section 7) | ownership and policy migration, with behavior changes isolated | `src/tests/windows_launch.rs`, `terminal.rs`, `bounded_read_process.rs`, `shared_codex.rs`, `shared_codex_events.rs`, and `host_command` wrapper-bypass/guard tests; mark legacy Windows transport coverage separately | With readiness barriers and retained identities, block a subscriber and prove root exit/cancel still closes the sole lease and observes owned descendants exit. A stale-generation cancel cannot hit a replacement runtime; one shared-Codex turn ending leaves another alive. Exercise native bounded-read deadline/output-limit and terminal cancellation through production paths, with each existing I/O policy preserved. |
| 12 | The watcher's consumers behind coalescing, with pending-batch admission and a maximum batch age (sections 4.4 and 4.11, point 4) | behaviour | file-change tests, carried-fence tests | The state lock is not taken per notify event. Deterministic tests: with the watcher paused between a batch's admission and its publication, a closing checkpoint gets an incomplete cut and withholds; under continuous events a batch is sealed at its maximum age and later events get a new admission; a cut through a fixed position completes once every admission through that position is resolved and its facts are installed, while newer admissions remain accounted for. Checkpoint authority still revalidates later interference before composing; completion of the earlier cut alone grants no credit |

**What depends on what.**

- Step 2 follows step 1 directly: it is the step that makes the first
  extraction's records mean something. It includes the receipt retention and
  credit-link reconciliation it requires, preserving any independently
  landed receipt fix.
- Steps 3 and 4 depend on the stream and not on durable records.
- Steps 5 and 6 depend only on the first extraction.
- Step 7 needs the source-root move of step 5.
- Step 8 adds only the remaining contract-field work and can land as a
  defect fix at any point.
- Step 9 needs step 2.
- Step 10a needs step 9 and its own entry conditions below; step 10b needs
  10a. The cut can be built and tested in the projection earlier; it becomes
  authority only in 10b.
- Step 11 depends on none of the others.
- Step 12 needs step 10b: until a complete cut is required, the watcher's
  consumers stay on the per-event path.

Where the order above is not forced by a dependency, it is the
coordinator's to change.

**Step 2, entry and exit.** Entry: extraction 1's provisional record/receipt path is integrated and the existing direct credit path remains authoritative. Exit: pending/start identity and original referenced context, then the exact terminal version, have Durable acknowledgements before credit depends on the execution record; typed persistence failure is tested; exact uncertain report replay and returned credit-id reconciliation are proven. Until exit, the target durability invariant remains unmet and no new authority or credit depends on provisional records.

**Step 11, entry and exit.** Entry: landed native launcher contracts and explicit bounded-read/terminal/runtime policies are pinned by tests, with the native deadline/output-limit gap filled for the migrated path. Exit: each in-scope host launch is admitted by the sole owner, cancellation is fenced by retained identity/runtime generation, no subscriber owns a job lease or mutable process handle, and no raw launch bypass remains. Preserve platform and containment limitations; each added policy or widened launch surface is separately specified and proved.

**Entry conditions for step 10a.** Overlap may be computed from the stream only
when the model carries all of the following, each with a test that shows the
same outcome as the marking it replaces:

1. A write's places can grow while it runs and can be "anywhere"; the shell's
   position is per-session state that decides later commands' places.
2. Another session is a writer for its whole turn, in its workdir's worktree
   and in its named root while it holds a grant. A read-only child never is.
3. The writer filter differs by rule: for a check in a turn, any other
   command of the session counts; for a carried check, the holder's own
   command is exempt only when it provably reads.
4. A reported edit names no path: it touches every open check of its own
   session, and other sessions' checks only in the editor's worktree.
5. A watcher batch is open to the left.
6. A check's window ends when both snapshots are ready; a carried check's
   window ends at the first terminal read of its run's record.
7. A turn start closes the session's commands left from the turn before; a
   forgotten command closes as "unknown end, overlapping everything after".
8. Writers active at a check's start are seeded even when their start was
   not seen or was evicted; a gap is conservative uncertainty.
9. The facts decided at a point stay rules over recorded facts and do not
   become intersections: the late-named root, the launch disposition, carry
   refusals, ambiguity, run attribution, expiry, and the withholding of
   successes the host cannot judge.

### 10.1 Step 3: the evaluation hold

**What it is for.** A criterion bound to a test passes only on a check that
is recorded at or before the evaluator's cut. A check the agent runs in a
turn is recorded at that turn's closing checkpoint, which is after any
request the agent makes in the same turn. So today an agent has to run the
test in one turn and ask for the evaluation in the next.

**The request cannot wait.** `termal_evaluate_acceptance` is a tool call
inside the requesting turn. Today it answers only once the request has
spawned an evaluator or produced a same-session brief. The turn cannot end,
and its checkpoint cannot happen, while the agent's runtime waits for that
answer. The hold is therefore never a wait inside the request: the request
answers at once and the evaluation is deferred.

1. **At the request.** The host does what needs no evidence: it checks that
   the task is open and has criteria, reads the policy and selects the mode,
   and refuses as it does today when any of that fails. The evaluation is
   deferred when, at the time of the request, the turn's grant has any check:
   one still open, one finished, or a gate launched in this turn and carried
   past it. A turn with none starts its evaluation at once, as today. A
   carried gate's result can only be recorded at a later checkpoint of the
   same claim, so the hold does not wait for it: the deferred answer says
   that the gate is still running, that its result will not be in this
   evaluation's basis, and that the evaluation goes stale when that result
   is recorded, as it does today.
2. **Deferred, with an evaluator.** In `independent_session` mode the host
   creates the evaluator delegation in a held state, persists it, and
   returns its delegation id at once. The response identifies the delegation
   as held for this turn's checkpoint and tells the caller to end the turn.
   Waiting while that turn remains open cannot release it; the normal
   delegation completion notification resumes work after the turn closes.
   The evaluator's session does not start while it is held.
3. **Deferred, in the same session.** In `same_session` mode the host
   persists a deferred request (its identity, its mode, and the turn, grant,
   run and task revision it was made under) and returns that identity at
   once, with no brief. After the checkpoint it queues a prompt to the
   requesting session, through the normal fenced prompt dispatch, that
   carries the brief and its basis. The brief and the basis are one
   immutable pair: a queued prompt delivered late never acquires a newer
   basis and never binds the request to the session's current run. If the
   pair has to be refreshed, the whole read is repeated and both are
   replaced.
4. **One slot, one delivery.** A held evaluator and a deferred same-session
   request each reserve the task's one active evaluation until they reach a
   terminal disposition. A repeated request or a repeated wake creates no
   second evaluator and no second delivery.
5. **Release.** A "checkpoint committed" observation wakes the hold. The hold
   verifies that the verification owner holds the receipt for that exact
   turn and grant. It then reads the run's work view from Engram
   (`work show … --json`), checks that the run in that view is the run the
   checkpoint's binding names, takes its top-level `evidence_basis`, builds
   the brief from that same read, and submits that basis. When a
   continuation page of the read is refused, the whole read starts again:
   the earlier attempt's pages and evidence are discarded with its basis,
   and the brief and the submitted basis come from the replacement read
   alone. Then it starts the held evaluator or queues the same-session
   prompt.
6. **When it does not release as recorded.**
   - A checkpoint that closed on a fallback releases the hold, and the brief
     and the requester's answer say that the checks were not recorded.
   - A checkpoint whose outcome is unknown is an unresolved write and does
     not release the hold. The hold waits for the replay that settles it.
   - The view shows another run, or the task was revised since the request:
     the deferred evaluation fails with that reason.
   - The turn ends without a checkpoint of that grant (a stop that closes it
     bare, a restart): the deferred evaluation fails with that reason.
   A deferred evaluation that fails is reported where its result would have
   been: as the held delegation's failure in the requester's fan-in, or as
   the queued prompt in the same session. It never starts an evaluation
   whose basis may miss the checks.
7. **Bound and recovery.** The hold's bound runs from the end of the
   requesting turn, not from the request: a turn may go on for as long as it
   likes after asking. Within the bound the hold either releases or fails
   with a reason. A lost wake is covered by the same bound: at it, the hold
   looks at the verification owner once more before it fails. A held
   delegation and a deferred same-session request are persisted before their
   id is returned. In both modes the persisted request retains its identity,
   mode, original turn, grant, run and task revision. After a restart each
   is reconciled under its original identity or failed with a surfaced
   reason; neither is silently lost or recreated as a new request.

## 11. Fenced authority operations

| Operation | Owner | Fence | When the outcome is unknown |
| --- | --- | --- | --- |
| Bind a session | `admission` | dispatch generation; one bind in progress; the settings-transition fence | The prompt is retained; the same request replays under the same key |
| Evaluate a turn | `admission` | the intent fingerprint, rechecked at queue promotion | As bind |
| Begin a turn | `admission` | the grant handed to the transport is recorded under the lock that authorises it | The grant is recorded as uncertain; only status or a fresh bind settles it |
| Claim the checkpoint | `verification` | grant id, runtime token, turn generation; one checkpoint in progress; its owner generation | The grant stays open; the next bind closes it with the cached report |
| Compose the report | `verification` | one critical section: take settled carried checks, validate fence and root generation, remove, compose; preceded by the drain, the seal and the cut through the seal (section 4.7, from step 10b) | What is not ready is withheld |
| Name, rename or clear a root | `source_root` | a live claim read from Engram; the host generation; the entry as it stood before the call | Nothing is named |
| Bind a named root to Engram (target) | `source_root` | the durable intent; the reporter's connection | Unconfirmed: no sighting, credit or evaluation source |
| Submit an acceptance evaluation | acceptance evaluation | `pending` acknowledged durable before the tracker runs; one submission at a time | The identical arguments are resent; only a receipt settles it |
| Release a deferred evaluation (target, section 10.1) | the evaluation hold | the request's identity, mode, turn, grant, run and task revision; the checkpoint receipt the verification owner holds for that turn and grant | The hold does not release; at its bound the deferred evaluation fails with a reason |
| Acknowledge persistence | the persistence writer | an exact record | The dependent operation does not proceed |
| Apply a settings transition | `settings` | the project's reset generation | Authority is retained and the save refused |
| Promote a queued prompt | turn dispatch | the evaluated intent fingerprint | The head stays queued |
| Close a job, cancel a launch, stop a runtime | process supervisor (section 7) | the immutable supervised process identity, the runtime generation, and exact lease ownership; one resume and one close | The cleanup status stays unknown and the same owned identity is reconciled; exit is never inferred from an accepted cancel, a process is never reopened by PID, and a newer generation is never cancelled |

## 12. Rejected alternatives, risks and open questions

**Rejected.**

- *A durable, global event log.* It would duplicate the execution record and
  Engram's store, and every consumer would need its own durable cursor. The
  stream is per process; recovery reads the owners.
- *The client stream as a consumer of the host stream.* Different consumers,
  payloads and overflow answers. They are siblings.
- *Interval overlap in the first extraction.* Today's marking covers cases
  that are not events inside a window (section 10, step 10a). Changing the
  algorithm and the ownership at once would leave no way to tell which broke
  a test.
- *Gating credit on durability in the first extraction.* Nothing about a
  check is durable on the host today; a gate would add a persistence wait to
  every turn close and a new way to lose credit, for no evidence gained.
- *A receipt synthesised from a read.* A read shows state, not the event
  that produced it.
- *Retracting reported credit for a write notify delivered late.* A batch
  admitted after a checkpoint's seal can describe a write made before it
  (section 4.7). Withdrawing a record a receipt already proves would need a
  contract operation Engram does not have; the residual is bounded by
  notify's delivery latency, equal to today's, and stated rather than
  closed. Reopening this needs both projects' coordinators.

**Risks.**

- The boundary of option 1 meets 804 field accesses in tests. If the test
  files cannot move into the component as pure moves, step 6 is slower than
  the steps before it suggest.
- Queuing deltas under the state lock (section 4.11) lengthens the lock's
  hold by a queue push per delta. The step measures it; if it is material,
  the alternative is a per-session publication order, which is a larger
  change.
- The first extraction's fixture (section 9.5) is compared byte for byte
  after placeholders, so it depends on a report that is otherwise
  deterministic for a given input. The values behind the placeholders (the
  root, the fingerprint that hashes it, the stamped times, and the revision,
  not shown equal across platforms) are checked on their own.
- Assembling a module from existing fragments by `include!` has not been
  compiled.
- The target keeps the rule that an unknown place counts everywhere and
  extends it: an observation that is admitted and not yet resolved counts as
  a possible write in every worktree, and from step 10b an incomplete cut
  withholds. A slow place resolution, or a pending watcher batch, in one
  session can then withhold another session's credit at its checkpoint,
  which is more conservative than today and is the shape of the third
  defect of section 2.9. The design answers with bounded resolution and
  typed reasons, not with fewer refusals. Step 10b therefore reports, for
  the checkpoints it handles, how many cuts were incomplete, for which
  reason, and how many checks were withheld that the marking of step 10a
  would have credited; a rate of cross-session withholding that is not
  explained by real overlap stops the step.

**Open.**

- Whether `pending_source_root_line` becomes a component of its own (host
  lines to the agent) or stays with `source_root`.
- The exact representation of retention pins between `source_root` and
  `verification` (section 5.4).
- Whether step 7 comes before or after step 6. It needs only the source-root
  move of step 5. Engram's planned claim-scoped read follows it.

## 13. Related work

Defects and work named above. None is solved by this brief; each is either
independent of the structure and may be fixed during the freeze, or is a
step of section 10. Each is tracked in the project's tracker; their
identifiers are recorded on this brief's own tracker item and not here,
where they would go stale.

- The evaluation hold (step 3).
- The streamed-prefix defect, client half, with the broadcaster's announced
  overflow (section 4.11).
- Lost check evidence is named to the holder, and the receipt's record ids
  are kept and compared (step 8).
- Evidence the evaluator's brief leaves out is counted and stated (landed
  after this brief's base).
- The named-root binding to Engram (step 7).
- A forgotten running command stops counting as a writer (section 2.6).
- A false refusal after a background launch (section 2.9).
- One session's unplaced command refuses every carried gate (section 2.9).
- A session that holds two claims is measured and evaluated for only one
  (section 2.9, two items).
- The watcher fence counts writes under `.tmp/` (section 2.9).
- The launcher's input fingerprint is unstable on CRLF checkouts (section
  2.9).
- The dormant authority-retirement subsystem is removed (section 2.7).
- Carried gates, known gaps, including the unbounded wait on the carried-poll
  lock (section 6.5).
- Check crediting: a check that ran alone and passed is sent as succeeded,
  and a downgrade names its rule.
- The native bounded-read validation gap (section 7.4).
- The intermediate-exit containment test asserts a transient state of its
  fixture (section 7.4).
