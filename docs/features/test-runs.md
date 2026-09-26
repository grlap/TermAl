# Test Runs

TermAl shows the test runs its agents start: which runs are active, their stages
and timings, their logs, and which sessions own each run or will be notified
when it finishes. Later slices let TermAl start, supervise and cancel runs
itself, and wake the requesting session when a run completes.

The runner is `scripts/test-launcher.mjs` (see [Testing](../test.md)). Its run
directories are the record of truth; TermAl mirrors them and never decides a
verdict itself.

**Status:**
- **Slice 1:** shipped (7f3c32c, with the PID-reuse fix in b77f1fa).
- **Slice 2:** approved by Greg (2026-09-25) and in implementation.
- **Slice 3:** waits on the restart-survival decision marked **Decision**
  below.

## Slices

1. **Visibility (read-only).** Index the launcher's run directories. Publish
   run summaries in the state snapshot and by live delta. Serve run detail and a
   bounded stage-log tail. Show a Test Runs tab and session markers. This covers
   runs started by agents through the CLI, including detached runs with
   `--notify`.
2. **In the conversation.** A persisted test-run card in the owning
   session's transcript, plus registered run waits: a session waits on runs,
   ends its turn, stays available to the user, and is resumed with one bounded
   result. This covers runs started through the CLI. It needs no launcher
   change, no host-started run and no cancel.
3. **Host-owned runs.** TermAl starts the foreground launcher as a hidden,
   supervised child. It then offers cancel, rerun, one run per worktree, live
   log follow (SSE) and boot reconciliation, with the agent tools
   `termal_start_test_run`, `termal_get_test_run`, `termal_list_test_runs` and
   `termal_cancel_test_run`. The card gets its cancel control here, not before.

## Guarantees kept from the launcher

These hold in every slice. TermAl relies on them and never weakens them.

- Success comes only from real process exits. A missing terminal result is
  `UNKNOWN`, never a pass.
- The input fingerprint is checked before and after the stages. Drift fails the
  run.
- The first failing stage stops the run; later stages stay `unrun`.
- Each run owns one directory below Git's `review-runs` metadata directory:
  `request.json`, `input.json`, `results.json` and one log per stage.
- A run is terminal when `results.json` has `state` in `{passed, failed}`, an
  `ended` time and an integer `exitCode`. This is the launcher's own
  `isTerminal`.
- `recover RUN_DIRECTORY` settles a run whose launcher died. It marks the run
  failed with `interrupted: true` and never reruns a stage.

## Slice 1: launcher fields

Slice 1 adds two fields to the launcher's `request.json`, written when the run
is created:

- `detached`: `true` for `--detach`, `false` otherwise.
- `creatorPid`: the pid of the launcher process that created the run.

For a foreground run that process also runs the stages. For a detached run it
waits for the worker's readiness, and the worker records its own pid in
`results.json` first. Runs created before these fields existed read as
`detached: null`, and are judged by `results.json`'s `pid` alone.

## Slice 1: model

### Where runs are found

For every local project, TermAl resolves the Git common directory of the
project root. It indexes `<common>/review-runs/*` for the main worktree and
`<common>/worktrees/*/review-runs/*` for linked worktrees. A Git common
directory is indexed once, however many projects share it.

A run's `projectId` is chosen in this order:
1. the project whose root equals the run's worktree (`request.json` `root`);
2. otherwise, the project with the longest root containing that worktree;
3. otherwise, the project with the smallest id that shares the common directory.

Remote projects are not indexed in slice 1.

### Identity and ordering

- The launcher makes a `runId` as `test-` plus a random UUID, so it is unique
  across projects and worktrees.
- If two directories carry the same `runId`, the first found in a sorted walk
  of the common directories is indexed and the other is ignored. Nothing is
  logged, since the rescan would repeat the line every few seconds.
- Lists are ordered by `startedAt` (newest first), then by `runId` ascending.

### Reading a run

- `request.json` and `results.json` are read with a size bound (1 MiB each).
  All fields are optional, and unknown fields are ignored.
- A directory without a readable `request.json` is skipped.
- The index keeps every non-terminal run, plus the newest 50 terminal runs per
  project. From slice 2 it also keeps any terminal run that a pending wait
  names, or that a card which has not yet stored its terminal snapshot names.
  So a long run that finishes behind 50 newer ones still settles its wait
  with its verdict, not as `UNKNOWN (not indexed)`.

### State

`state` is exactly one of:

- `passed` or `failed`: `results.json` is terminal. The value is copied from
  it. This includes runs settled by `recover`, which are `failed` with
  `interrupted: true`.
- `running`: `results.json` is readable and not terminal, and a responsible
  process may be alive. That process is `results.json`'s `pid` when present,
  otherwise `request.json`'s `creatorPid`. "May be alive" is the launcher's
  `processMayBeAlive` rule: only proof of exit counts. A reused pid is also
  proof. The process that wrote a pid into a file existed when it wrote it. So
  if the live process at that pid was created more than 2 s after the carrying
  file (`results.json` for `pid`, `request.json` for `creatorPid`) was last
  modified, it is not the run's process. That case reads `unknown`, never
  `running`, for runs recorded before any launcher change as well. The pid and
  the modification time always come from the same version of the file, so a
  replacement during the read cannot pair a new pid with an older time. Creation
  times are read on Windows, Linux and macOS; where one cannot be read, or a
  modification time leaves no room for the margin, the pid rule alone applies.
  Both times are wall-clock times. A clock step can therefore make a live run
  read `unknown` until its next write. On Linux, a forward step moves the boot
  time that creation times are computed from. On any platform, a backward step
  of more than 2 s between the writer's start and its last write has the same
  effect. The error is only ever `running` shown as `unknown`, never a wrong
  verdict. A start identity recorded by the launcher itself would remove it
  (tm-fa5e).
- `unknown`: everything else that is not terminal. That covers an unreadable
  `results.json`, a responsible process proven gone, or no pid recorded
  anywhere. A missing pid is never `running`. A launcher writes its terminal
  results just before it exits, so a process found gone, or no pid found, is
  never judged from a `results.json` read made before that check. The index
  reads the results once more after the check and judges from that read. A
  run that finished between the two steps therefore reads `passed` or
  `failed`, not `unknown` for a rescan.
  - If the second read names another responsible process, it was made before
    that process's check, so it is judged the same way again. That happens
    when a detached run's creator hands over to its worker. The number of
    such reads per rescan is bounded.
  - A second read is final for the process it found gone, so an unchanged
    abandoned run is not read again on every rescan.
  - If the second read fails, that rescan keeps the verdict of the first read,
    and the next rescan reads again. From slice 2 such a verdict is `unknown`
    with `unknownReason: resultsUnreadable`, which never settles a wait.

`interrupted: true` is copied from `results.json` and only ever appears with
`failed`. An `unknown` run has not been settled; `recover` settles it. TermAl
never writes to a run directory in slice 1.

### Refresh

A host thread rescans the run directories: every 2 s while any indexed run is
`running`, and every 10 s otherwise, so an abandoned `unknown` run does not
hold the fast rescan until someone recovers it. From slice 2, a changed
directory also triggers a rescan at once (see Noticing runs quickly). Rescans
are serialized: one runs at a time, from its first read to its last commit.
TermAl's workspace file watcher deliberately ignores `.git/`, where the runs
live.

A rescan only stats `request.json` and `results.json`. It parses a run again
only when their size or modification time changed, and it re-checks liveness
only for non-terminal runs. Clients never poll; they receive deltas.

The launcher replaces both files by atomic rename, but a read can still lose a
race with a replacement (on Windows, opening a file mid-rename can fail). A
read fails when the file is unreadable, not valid JSON, or missing while its
run directory is still listed. One failed read cannot tell a race from a file
that is malformed or gone for good, so the policy is a bounded grace on stale
evidence. After a good read, one failed rescan keeps that read, sends nothing,
and reads again. If that read fails too, the stale evidence is given up:
unreadable results leave the run `unknown` with no stages, and an unreadable
request drops the run (`testRunRemoved`). An earlier `passed` is never shown
for more than one rescan of unreadable results. A failed read is never cached:
every rescan tries again until the file reads, whatever its size and time say.

A file larger than 1 MiB is not a race and gets no grace: oversized results
leave the run `unknown` at once, an oversized request keeps the run out of the
index, and neither is read again until the file changes.

The rescan caches every run directory it reads, but never a parsed file. From
`results.json` it keeps a bounded extract: the digest (`detailVersion`), the
terminal state, the first 64 stage summaries (so a summary's `stages` can be
shorter than the plan, while `currentStage` names the running stage wherever
it is), `interrupted`, `ended`,
`exitCode`, the pid, and the error cut to 512 bytes. Nothing that names
something is ever cut, since a cut identifier could alias another. A run whose
run id, `owner` or `notifyTo` exceeds 128 bytes, or whose worktree or (from
slice 2) run directory exceeds 4096 bytes, is not indexed. A stage whose name is not a launcher stage name
(`^[A-Za-z0-9_-]+$`, at most 128 bytes) is left out of the summary. A timestamp
over 128 bytes reads as null. So what the index keeps per run is bounded,
listed or not; the total still grows with the number of run directories. A run
that comes back into the list needs no new read. The detail and log routes read
the file afresh, so the detail shows every stage. If a rescan cannot
record all its deltas, the runs whose delta was not sent keep their previous
entries, so the next rescan sends them.

The summary's `detailVersion` comes from the rescan's read, and the detail
route reads the file again, so a detail fetched just after a replacement can
be newer than the summary's version. The client then refetches once more when
the next summary arrives. The two are not guaranteed to come from the same
bytes.

## Slice 1: wire types

All times are strings exactly as the launcher writes them: ISO 8601 UTC from
JavaScript's `toISOString()`, for example `2026-09-25T15:37:01.000Z`. A value
TermAl has not seen is `null`, never omitted.

```ts
type TestRunState = "running" | "passed" | "failed" | "unknown";
type TestRunStageState = "unrun" | "running" | "passed" | "failed";

interface TestRunStageSummary {
  name: string;                 // ^[a-zA-Z0-9_-]+$
  state: TestRunStageState;     // copied from results.json
  exitCode: number | null;
  startedAt: string | null;
  endedAt: string | null;
}

interface TestRunSummary {
  runId: string;                // "test-" + UUID
  projectId: string | null;
  worktree: string;             // request.json root, forward slashes
  runDir: string;               // absolute, forward slashes
  preset: "full" | "focused" | "live";
  command: string[] | null;     // focused runs only: argv, at most 32 items
                                // and 512 bytes in total
  commandTruncated: boolean;
  detached: boolean | null;     // null: run predates the field
  state: TestRunState;
  interrupted: boolean;
  currentStage: string | null;  // the stage whose state is "running"
  stages: TestRunStageSummary[];
  ownerSessionId: string | null;   // request.json owner, when it is a known session
  notifyTo: string | null;         // raw --notify target
  notifySessionId: string | null;  // notifyTo resolved by id or name
  startedAt: string | null;
  endedAt: string | null;
  exitCode: number | null;
  error: string | null;         // results.json error, at most 512 bytes
  detailVersion: string | null; // opaque content digest of the results.json
                                // read; null without readable results
}

interface TestRunDiagnostics { text: string; truncated: boolean }

interface TestRunStageDetail extends TestRunStageSummary {
  command: string[] | null;     // argv as recorded by the launcher
  cwd: string | null;           // absolute, forward slashes
  log: string | null;           // path relative to runDir, forward slashes
  diagnostics: TestRunDiagnostics | null;
  error: string | null;
}

interface TestRunPreflight {
  name: string;
  command: string[] | null;
  exitCode: number | null;
  log: string | null;
  diagnostics: TestRunDiagnostics | null;
}

interface TestRunDetail {
  stages: TestRunStageDetail[];
  preflight: TestRunPreflight[];
  expectedFingerprint: string | null;
  before: string | null;
  after: string | null;
  limitations: string | null;
}
```

`preset` is `full` when `request.json` says `full`, `live` when it carries
`liveEngram`, and `focused` otherwise. A stage that is `running` inside an
`unknown` run is displayed as interrupted. No summary carries diagnostics text
or log content.

### Snapshot and delta

- `StateResponse.testRuns: TestRunSummary[]`, in list order. The field is
  omitted when empty, like `delegationWaits`.
- `DeltaEvent` `testRunChanged { revision, run: TestRunSummary }` is sent when
  the run's summary changes, including the host verdict and `detailVersion`.
  `detailVersion` is an opaque digest of the `results.json` bytes the summary
  was read from, and the detail is built from the same file. It is stable
  across rescans and host restarts while the file's content is unchanged.
  It is null when there are no readable results, so an earlier version is not
  reused after results are given up. A rescan re-reads the file only when its
  size or modification time changes (see Refresh), so a replacement that keeps
  both is not seen, as for the rest of the index. A client showing a run's
  detail refetches it when the run's `detailVersion` differs from the one it
  fetched against, whether the summary came in a delta or in a snapshot. So a
  snapshot that consumed the delta's revision cannot hide a stale detail.
- `DeltaEvent` `testRunRemoved { revision, runId }` is sent when a run leaves
  the index: its directory was removed, or it aged out of the newest 50.
- Deltas use the same revision rules as every other state delta. A snapshot
  received on connect or resync replaces the whole `testRuns` list.

## Slice 1: HTTP

- `GET /api/test-runs?projectId=ID` returns `{ runs: TestRunSummary[] }` in
  list order. Without `projectId`, it returns all indexed runs.
- `GET /api/test-runs/{runId}` returns
  `{ run: TestRunSummary, detail: TestRunDetail }`.
- `GET /api/test-runs/{runId}/stages/{name}/log?tail=BYTES` returns
  `{ text: string, truncated: boolean, size: number }` as JSON.
  - `size` is the log's size in bytes.
  - `text` is its last `BYTES` bytes (default 64 KiB, at most 1 MiB), decoded
    as UTF-8 with invalid sequences replaced. The cut is moved forward to a
    character boundary.
  - `truncated` is true when the log is longer than the tail.
- `{name}` must match `^[a-zA-Z0-9_-]+$`. Only the log path recorded in
  `results.json` is opened. Both that path and the run directory are
  canonicalised first, which resolves symlinks and junctions. The path must lie
  inside the canonical run directory; otherwise the route returns 400.
- An unknown `runId` or stage returns 404. A stage without a log returns 404.
- Detail and log routes read `results.json` on each request. When it exists
  but cannot be read whole or parsed, they return 409 rather than an empty
  detail; retry. When it is larger than 1 MiB they return 422, which a retry
  cannot fix. A run with no `results.json` yet has an empty detail.
- Every route is read-only in slices 1 and 2. Live follow (SSE) comes in
  slice 3.

## Slice 1: UI

The UI is Termal::Codex's, built against this contract. Its beads are
tm-ncc6.7.1 (tab wiring), tm-ncc6.7.2 (list, detail, log tail) and tm-ncc6.8
(markers). The server side and this document are Termal::Opus's.

- **Test Runs tab.** Kind `testRuns`, following the Work tab pattern: factory,
  validation, label, dock action "Open Test Runs", and one tab per pane.
  - The list shows a state pill, preset (or the focused command), worktree,
    owner session, notification target, started time, duration, and the
    current stage while running.
  - The list can be filtered by state and by session.
  - Deltas update the list; there is no polling.
- **Run detail.** Every stage with state, exit code and duration.
  - `unrun` and interrupted stages are shown explicitly.
  - It shows the first failure's diagnostics, the run directory with a copy
    button, and the fingerprint.
  - A stage-log tail is available, with a refresh button.
  - The detail refetches on `testRunChanged` for its run.
- **Session markers.** Derived client-side from runs whose `state` is
  `running`, in three roles:
  1. owner of a run with `detached: false`: "running tests";
  2. owner of a run with `detached: true` or `null`: "test run in background";
  3. `notifySessionId` of a run: "test run will notify this session". This
     states the notification target only. It does not claim the session is
     idle or waiting; registered waits come in slice 2.

  A session shows one marker, for the first role in that order that applies to
  it. Its text is the role's label, then:
  - with more than one run in that role, the count, for example
    "running tests (2)";
  - with exactly one, ` · ` and that run's `currentStage` when it has one, for
    example "running tests · rust-tests", and nothing more otherwise.

  Clicking it opens the Test Runs tab filtered to that session. The marker
  disappears when no run in that role is `running`. In a session pane it sits
  in the pane's existing toolbar strip, cut to 16rem with the full label as its
  title, and adds no row: the transcript, the activity strip and the composer
  keep their layout. It also shows on the board.
- The prototype linked on the tm-ncc6 epic is the visual reference.

## Slice 2: the test-run card

A test run is a specialized tool execution, so its card belongs to the
command-card family. It is the long-running execution view: an agent launches
the run detached and registers a wait. Its own command card finishes in
seconds with the launch receipt, and the test-run card that follows carries
the execution.

### When a card is created

- On its first boot with slice 2, TermAl stores `testRunCardsEpoch`, the
  current time, in persisted state. That state is persisted before the first
  rescan of the boot publishes any run. The epoch is never derived from "now"
  again.
- A card is created when the index sees a run that meets all of these:
  - its `ownerSessionId` resolves to a session that is not archived;
  - its `request.json` `started` is at or after the epoch;
  - it is non-terminal at first sight, or it started at most 10 minutes
    before first sight. The second case catches a short focused run that
    ended between two rescans.
- "First sight" is the index's first observation of the run in this host
  process. So after a restart, a post-epoch run that ended while TermAl was
  down still gets its card if it started within 10 minutes of that boot's
  first sight. A longer one does not; it stays in the tab.
- A card is keyed by (owner session, runId). Persisted state holds a
  `testRunCards` map, from runId to `{ sessionId, messageId }`, the way
  delegation records point at their card message. The transcript stays the
  render source. Boot rescans, index churn and ageing-out never create a
  second card, and boot never scans transcripts to find cards.
- Runs from before the epoch never get a card; they stay in the Test Runs tab.
- Only the owner gets a card. A `--notify` target alone gets none, because a
  notification is not a registered wait. A run started outside TermAl, with a
  null `owner`, gets none.
- The card is appended to the end of the owner's transcript when TermAl first
  sees the run, like a delegation card at spawn. It keeps its id and position
  and is updated in place.

### Card rendering

- The header is the launcher invocation: `full`, `live`, or the focused argv.
- The status visual is shared with command cards. `running` is running,
  `passed` is success and `failed` is error. `unknown` has its own neutral
  treatment, because it is never success.
- While the run is `running`, the card shows the activity marker.
- The body shows stage progress and, for a failed run, the failure excerpt.
- The renderer reads the verdict and progress only from `message.run`. It
  never joins the live index, so there is one verdict source and historical
  cards do not rerender.
- The "details" action is always enabled and opens the run in the Test Runs
  tab. When the runId is not in the index, the tab shows an explicit "run no
  longer indexed" view with the runId, run directory and the `summary`
  command.
- A card never shows `passed` without a terminal `results.json`. A dead worker
  shows `unknown` with its reason and names `recover` as the way to settle
  it.
- There is no cancel control before slice 3.
- A possible later refinement: fold the launching command card into the
  test-run card when its output carries the run's `RUN` or `STARTED` receipt.

### Card wire type

A new message variant. It reuses neither `parallelAgents` nor `command`,
because its states (`unknown`, `interrupted`) are neither delegation states
nor command states, and the host updates it in place from the index.

```ts
interface TestRunCardMessage {
  id: string;
  type: "testRun";
  timestamp: string;            // creation; never changes on update
  author: "system";
  schemaVersion: 1;
  run: TestRunCardSnapshot;
}

interface TestRunCardSnapshot {
  // Core: never cut.
  runId: string;
  worktree: string;
  runDir: string;
  preset: "full" | "focused" | "live";
  detached: boolean | null;
  state: TestRunState;
  unknownReason?: TestRunCardUnknownReason | null; // only when state is unknown
  interrupted: boolean;
  currentStage: string | null;
  startedAt: string | null;
  endedAt: string | null;
  exitCode: number | null;
  // Optional part: filled in priority order within its budget.
  stages: TestRunStageSummary[];
  stagesOmitted: number;        // stages dropped by the budget; 0 when none
  error: string | null;         // at most 512 bytes
  errorTruncated: boolean;      // shorter than the results.json error
  failure: TestRunCardFailure | null; // terminal failed runs only
  command: string[] | null;     // same bounds as TestRunSummary
  commandTruncated: boolean;    // cut by the summary bounds or dropped
}

interface TestRunCardFailure {
  phase: "stage" | "preflight";
  name: string;                 // the first failing stage or preflight check
  excerpt: string;              // its diagnostics text, at most 4 KiB
  truncated: boolean;
}

type TestRunUnknownReason = "processGone" | "resultsUnreadable" | "noPid";
// notIndexed: the run left the index before a terminal result.
type TestRunCardUnknownReason = TestRunUnknownReason | "notIndexed";
```

**Size.** Every budget is measured in UTF-8 bytes of the serialized JSON,
escaping included.

- **Core: at most 10 KiB.** The core is never cut, so no identity or path is
  truncated. The raw limits keep an ordinary run far below 10 KiB: runId 128
  bytes, worktree 4096 bytes, and 128 bytes for each timestamp and
  `currentStage`. From slice 2, a run whose runDir exceeds 4096 bytes is not
  indexed. A run whose serialized core would still exceed 10 KiB, which is
  only possible with heavily escaped paths, gets no card; it stays in the
  tab.
- **Optional part: at most 8 KiB.** It is filled in this fixed order, so the
  same run always gives the same snapshot:
  1. the failed stage and the running stage, if any;
  2. `error`;
  3. `failure`, whose excerpt is cut at a UTF-8 character boundary with
     `truncated: true`;
  4. `command`, kept whole or dropped whole; dropping sets
     `commandTruncated`;
  5. the remaining stages in plan order. Those that do not fit are dropped
     from the end and counted in `stagesOmitted`.
- **Total: at most 18 KiB.** A typical card is under 2 KiB.
- **What gives way.** The stage list is the elastic field, because 64 stage
  rows alone can exceed 8 KiB. The failure excerpt outranks the rows of
  stages that passed, which the Test Runs tab still shows.
- **Cutting.** A field is cut or dropped, never sliced as raw JSON. The
  snapshot never holds log bodies.
- **Preflight failures.** A preflight failure keeps its check name and
  `error`, so the card stays understandable without any stage.

`errorTruncated` is measured against the original `results.json` error, so it
also reports the index's 512-byte extract.

`unknownReason` is added to `TestRunSummary` as well, as an additive change
sent only when set. In the UI types it is optional, and a missing reason
means "not known", never `processGone` or `noPid`.

### Card updates

- **When the host writes.** On every change to the run's summary, the host
  rebuilds the snapshot and persists and publishes it only if it differs from
  the stored one.
  - The snapshot has no volatile fields: no `detailVersion`, and no elapsed
    time, which the client computes from `startedAt`.
  - So there is no per-second write, and a `detailVersion`-only change
    rewrites nothing.
  - A late `error`, richer diagnostics or a stage timing does update the
    card.
- **The failure excerpt.** The host reads it from `results.json` with the
  detail route's 1 MiB guard. It does so when the run is terminal `failed`
  and its `detailVersion` differs from the one the stored excerpt came from.
  The card keeps its last stored snapshot, so the run's evidence can later
  leave the index or be pruned without emptying it.
- **Delta.**
  - The card is created with the existing `messageCreated`.
  - Updates use `testRunCardUpdated { revision, sessionId, messageId,
    messageIndex, messageCount, preview, sessionMutationStamp?, run }`.
    `preview` is required and `sessionMutationStamp` optional, as on
    `ParallelAgentsUpdateEvent`, with the same retained-message and resync
    fallback.
  - `run` replaces the snapshot whole. `id`, `timestamp`, `author` and
    `schemaVersion` never change.
  - A delta at or below the applied revision is ignored.
- **Non-terminal cards after a scan.** A persisted card that is not terminal
  is checked against the index after every full scan, including the first
  scan after boot. The run's current summary updates it: terminal results, a
  live responsible process, or `unknown`.
  - A run no longer in the index becomes `unknown` (`notIndexed`), so no card
    stays `running` after its directory is gone.
  - If the run is indexed again, its summary updates the card again.
- **Limits of the pid evidence.** The PID-reuse rule (see State) proves only
  that the process that wrote a pid existed when its file was written. It
  does not exclude a process created within the 2 s margin, or after
  something else touched the file. Where no creation time can be read, the
  pid rule alone applies. In those cases a reused pid can still read
  `running` until other evidence arrives.

### Classifying `processGone`

`processGone` and `noPid` are never classified from a `results.json` read
made before the liveness check (see State). Without that rule, a launcher that
writes terminal results and exits between the read and the check would settle
a wait with `UNKNOWN` for a run that passed, and write two card revisions.

When the read after the check fails, the verdict is `unknown` with
`resultsUnreadable`, not `processGone`. It is not settled, and the next rescan
reads again. Results that cannot be read at all, are missing or exceed the
read limit are `resultsUnreadable` too. The one-rescan grace for a failed read
(see Refresh) keeps the last good results together with what was confirmed
about them, so a known reason does not flicker over one failed read. When the bounded reads after checks
run out on a run still handing over from process to process, the verdict
rests on a read made before the last check, so it carries no reason.

### Noticing runs quickly

The rescan stats each tracked `review-runs` directory every 2 s. A changed
directory modification time triggers a full rescan at once, so a new run is
seen within about 2 s. The tracked directories are:

- every `review-runs` directory, including one that does not exist yet, whose
  creation counts as a change;
- the Git `worktrees` directory, whose modification time changes when a
  linked worktree is added, so that worktree's `review-runs` is tracked from
  the next rescan;
- a run directory whose files have not all arrived. The launcher creates the
  directory, then renames `request.json` and then `results.json` into it. A
  directory skipped for want of a readable request, or a run indexed before
  its results, stays tracked until they read.

A run that cannot be indexed as its files stand (an oversized request, or an
identifier or path over its bound) is not tracked, since the launcher's own
writes to it would trigger a rescan on every tick. The per-run liveness
cadence is unchanged. The 10 s full rescan stays as the backstop for a change
made in the same modification-time tick as a stat.

## Slice 2: run waits

A session waits on runs the way it waits on delegations. It registers the
wait and ends its turn, and the user can keep talking to it. When the runs
settle, TermAl queues one resume prompt with the result.

The wait itself is not a transcript entry, as for delegation waits. A pending
wait is the persisted record plus the pane and board indicator. The resume
prompt is the only thing it puts in the transcript.

### Wait tool

`termal_resume_after_test_runs { runIds: string[], mode?: "all" | "any",
title?: string }` is a sibling of `termal_resume_after_delegations`, not an
extension of it. The delegation tool's validation and resume prompt are part
of the reviewed `/review-changes` contract.

- **`runIds`.** One to 16 distinct ids, each indexed and in the caller's
  project.
  - The caller need not be the owner: a coordinator may wait on a run it did
    not start.
  - If an id is not in the index, the tool first forces one synchronous
    rescan of the caller's project roots, bounded to 1 s. Only then does an
    unknown or foreign id reject the whole call. So an agent that registers
    right after launching a detached run is never pushed into polling.
  - Rescans are serialized. The tool's forced rescan and the background
    rescan take the same rescan lock, so a scan that started earlier never
    commits over a newer one.
  - The wait's mandatory prompt content, meaning every run's header and
    commands, must fit in 48 KiB. A registration that would exceed that is
    rejected, which leaves at least 16 KiB for excerpts.
- **`mode`.** Defaults to `all`.
- **Result.** `{ waitId, runIds, mode }`.
- **Already settled.** A wait on runs that have already settled queues its
  resume at once and never blocks. This is also how a session re-fetches a
  verdict.
- **No mixed waits.** A wait cannot mix delegations and runs.

### Settled

A run is settled for a wait when it is `passed` or `failed`, or `unknown` with
`unknownReason` of `processGone` or `noPid`. It is also settled when it leaves
the index after the wait was registered, and it then resumes as `UNKNOWN (not
indexed)`.

An `unknown` run with `resultsUnreadable`, or with no reason, is not settled:
the run may still finish, and the index retries.

### Wait lifecycle

- **Identity.** A wait is identified by its `waitId`.
  - The same run may be in several waits.
  - A session may register again after a resume or a Stop.
  - Waits are never deduplicated by run. The one-per-(owner session, runId)
    rule applies to cards only.
- **Record.** `TestRunWaitRecord` is a new persisted record type with the
  lifecycle of `DelegationWaitRecord`, which is not widened.
  - It exists only while pending.
  - Resuming or consuming it deletes it, and `testRunWaitConsumed` reports
    the reason.
  - It reuses the delegation-wait machinery: refresh on index change, the
    same prompt queue and dispatch rules, and boot reconciliation.
- **Stop or archive.** A Stop or an archive consumes the session's pending
  run waits. Runs and cards stay, and nothing reactivates on its own. An
  archived session gets no new card and no wait resume.
- **Restart.** Pending waits are reloaded. After the first index scan, a wait
  whose runs have settled resumes once, and only for an idle session that is
  not latched.

```ts
type TestRunWaitMode = "any" | "all";

type TestRunWaitRecord = {
  id: string;                   // waitId
  sessionId: string;            // the waiting session
  runIds: string[];             // registration order, 1 to 16
  mode: TestRunWaitMode;
  createdAt: string;
  title?: string | null;
  runs: TestRunWaitRunLabel[];  // same order as runIds; captured at
                                // registration, never updated
};

type TestRunWaitRunLabel = {
  runId: string;
  runDir: string;               // captured so a resume after the run left the
                                // index still names its evidence
  preset: "full" | "focused" | "live";
  worktree: string;
  ownerSessionId: string | null;
  startedAt: string | null;
};

type TestRunWaitConsumedReason =
  | "completed"
  | "sessionStopped"
  | "sessionUnavailable"
  | "sessionRemoved";

type TestRunWaitCreatedEvent = {
  type: "testRunWaitCreated"; revision: number; wait: TestRunWaitRecord;
};
type TestRunWaitConsumedEvent = {
  type: "testRunWaitConsumed"; revision: number; waitId: string;
  sessionId: string; reason: TestRunWaitConsumedReason;
};
type TestRunWaitResumeDispatchFailedEvent = {
  type: "testRunWaitResumeDispatchFailed"; revision: number;
  sessionId: string; error: string;
};
```

`StateResponse.testRunWaits?: TestRunWaitRecord[]` lists pending waits only,
and is omitted when empty. There is no "changed" event: a wait's fields never
change after creation.

### Resume prompt

There is one prompt per wait, never repeated log or card messages. Its overall
cap is 64 KiB. For each run it contains:

- a header: runId, preset, verdict (`PASS`, `FAIL` or `UNKNOWN (reason)`),
  `interrupted`, exit code, start and end times, and the run directory;
- for `FAIL`, the first failing stage with its diagnostics excerpt; for
  `UNKNOWN`, the stage that was running at the last observation;
- the commands to inspect it: `node scripts/test-launcher.mjs summary
  RUN_DIR`, plus `recover RUN_DIR` for `UNKNOWN`.

Headers and commands are never cut; registration bounds them to 48 KiB. Each
run's excerpt budget is min(8 KiB, (64 KiB minus all headers and commands) /
number of runs), and never negative. `UNKNOWN` is never phrased as a pass.

### Waiting indicator

The indicator sits in the session pane's waiting indicator, next to delegation
waits, and on the board. It is not a transcript message. For example: "waiting
for test run RUN (full, stage rust-tests, 3m12s)".

- It shows only while a wait is registered. It is never inferred from a
  running card, and it hides while the session is actively responding.
- While the run is in `testRuns`, the indicator joins that run's live summary
  for its state, current stage and elapsed time. After the run leaves the
  index, it falls back to the wait's `runs` label. So a waiter that does not
  own the run, and therefore has no card, is still labelled.

## Slice 3: decisions and contract

- **Decision (restart survival).** Proposed: option B. Test runs get a
  dedicated job object without kill-on-close. A TermAl restart then leaves the
  run alive, and TermAl re-attaches it by pid plus process creation time.
  Cancel still terminates the job. The alternative, option A, keeps
  kill-on-close: every TermAl restart then makes the run `unknown`.
- **Stop.** Decided in slice 2: a user Stop consumes pending run waits, and
  never cancels a run.
- **Admission.** One run per worktree. That means no non-terminal host record
  for the root, and no indexed `running` run.
- **Cancel.** Refused (409) for runs TermAl did not start.
- **Rerun.** Rebuilds the CLI invocation from `request.json` and creates a new
  run. It is refused (409) while a run is active for the worktree.
- **Remote.** Starting, cancelling or rerunning a run for a remote project
  returns 501.
- **Retention.** Waits for the evidence retention policy in tm-uex8.9. Running
  and `unknown` runs are never pruned.
- **CLI.** `--detach --notify` stays for coordinators outside TermAl.

## Failure taxonomy

| Event | Evidence | Shown as |
| --- | --- | --- |
| Stage exits non-zero | Terminal `results.json` | `failed` |
| Launcher killed (agent session ended, TermAl restart, reboot) | Non-terminal `results.json`, responsible process gone | `unknown` until `recover`, then `failed` with `interrupted` |
| Drift before or during the run | Terminal `results.json` with a drift error | `failed` |
| Notification or wake lost | Terminal `results.json`, no delivery receipt | Verdict unchanged; delivery is re-derived, never rerun |

## Related

- [Testing](../test.md): launcher contract, `recover`, receipts.
- [Agent Delegation Sessions](./agent-delegation-sessions.md): waits and fan-in,
  the model for the slice 2 run waits and the card.
- [Durable Agent Mailboxes](./agent-mailboxes.md): `--notify` delivery for
  coordinators outside TermAl.
- [Work Visualizer](./work-visualizer.md): the tab pattern the Test Runs tab
  follows.
