# Test Runs

TermAl shows the test runs its agents start: which runs are active, their stages
and timings, their logs, and which sessions own each run or will be notified
when it finishes. Later slices let TermAl start, supervise and cancel runs
itself, and wake the requesting session when a run completes.

The runner is `scripts/test-launcher.mjs` (see [Testing](../test.md)). Its run
directories are the record of truth; TermAl mirrors them and never decides a
verdict itself.

Declared test commands (`termal-tests.toml`, TRX/JUnit artifacts) use the existing
artifact adapter: see [Declared Test Commands](declared-test-commands.md).

## Shared registration and command visibility

The global Test Runs list and left-rail active-run badge include both launcher runs and
recognized provider commands, including declared tests. A command appears when
its start is observed, before its result artifact exists. If a provider supplies
the command text later, the row retains the original runtime/tool identity,
start time and work ownership; that late description creates no retroactive
verification check. Visibility is independent of the owner's active or open
session. Unknown PIDs, stages and locations remain unknown.

The flask icon shows the active-run count after the first state snapshot, with
the same count in its accessible label and tooltip. Zero active runs means no
badge. Clicking the icon opens all projects. Per-run status and restart evidence
impact remain in the Test Runs panel; there is no persistent global-header chip.

`POST /api/sessions/{sessionId}/test-runs` accepts `{runId, runDirectory}` for an
exact launcher request in the local project's Git run directories. Its successful
response identifies `contract: "termal-durable-test-run-registration"`, `version: 1`
and `registered: true`. The host acknowledges only after content persistence and
query visibility. Identical retries retain one record; changed immutable request
identity is refused. Registration failure never returns a durable ACK.

Registration validates only the named run's canonical Git-admin parent and
request. It does not enumerate historical runs or wait for the global artifact
rescan. The four-field ACK stays unchanged. An immediate exact-id GET returns
the durable registration with `source: "registration"` until the background
index catches up; indexed detail has `source: "index"`. This fallback confirms
launch identity and durability, not execution results. Cards and SSE continue
through the background scanner, normally on its next two-second tick.

Indexed detail is selected only when its native canonical locator and immutable
request identity (run, digest, owner and start) match the captured registration.
A copied or older indexed request uses the registration fallback. If that
registration changes during the off-lock path check, GET returns 409 for a fresh
query; it does not select against a different registration snapshot.

`GET /api/test-run-diagnostics?runId=EXACT_ID` (or `operationId=EXACT_ID`)
returns bounded, memory-only registration phase observations and overlapping
rescan traces. Supplying both identifiers narrows the match. At most 256
operations with 32 phase points each are retained, including a truncation flag;
old or evicted traces are absent. Timestamps are microseconds from one
host-local monotonic epoch, identified by `serverInstanceId`, and reset on
restart. Traces outlive an aborted HTTP caller but grant no test credit and do
not survive restart. `response_ready` means the handler prepared its result,
not that the caller received it. No request bodies, paths, commands or error
details are stored in this ring.

`GET /api/test-run-diagnostics?recent=rescans` discovers recent background
rescans without a registration or a previously known operation ID. It returns
only rescan metadata, newest first, with a default limit of 20. `limit=N`
accepts 1 through 256 and applies only to this discovery mode. Discovery cannot
be combined with exact `runId` or `operationId` filters; use the returned IDs
in a separate exact query. `selectionLimit` reports the applied discovery limit
(null for exact queries), and `selectionTruncated` says additional matching
traces remain in the current ring. It does not report older traces already
evicted from the shared 256-operation ring. Each trace's `truncated` flag still
describes its own phase-point limit. In-flight traces may lack later marks;
an empty response does not prove that no rescan ran. A host restart changes
`serverInstanceId` and resets the ring and its monotonic timing epoch.

For a discovered rescan, `reads_done` to `commit_acquired` measures the wait
to acquire the commit-section StateInner guard. `commit_acquired` to
`published` measures the instrumented guarded span, not the exact full lock
hold: the guard is released after that mark. Missing or truncated marks cannot
establish a duration or prove absence of a long hold. Discovery reads only the
existing trace ring; it grants no verification and changes no admission,
state authority, persistence or test execution.

The production launcher requires this versioned ACK and an exact durable run
query before fingerprint capture, preflight, Cargo bootstrap or any stage.
Foreground and detached launches fail with an explicit registration error when
the host configuration, acknowledgement or query cannot be confirmed. A lost
POST reply may be retried with the same run identity and immutable request;
definitive refusals and malformed ACKs are not retried. A detached worker checks
the saved receipt, immutable request and host visibility again before preflight.
Each exact confirmation GET permits at most one reissue when the transport
reports `ECONNRESET` before `fetch` delivers a response. Both attempts share the
original abort signal and eight-second total deadline; the GET cannot replay
the registration POST. HTTP status, response-body, identity and abort failures
remain terminal, as does a second reset. Diagnostics retain `queryAttempt` and
`transportResets`, including when the second read succeeds. This is an
idempotent-read contract, not proof of a stale pooled socket or of zero bytes
received internally by the transport. Both reads must confirm the same exact
durable registration before any stage can run.
Only a standalone invocation with no `TERMAL_BASE_URL`, `TERMAL_SESSION_ID` or
`TERMAL_CLI` environment variables may execute as `UNTRACKED`; a partial context
fails closed. Registration and its receipt never establish test credit.

Rows distinguish `registered`, `hostObserved` and `discovered` provenance. An
observed command merges with a launcher row only through its explicitly returned
run locator and matching immutable request identity, or the existing validated
terminal digest. Time, PID or similar command text never chooses a run. An exact
merge lists `supersededRunIds` so the live client replaces provider-only rows in
one update; losing that proof restores the original durable observation. Command
start timestamps are labelled separately from launcher request timestamps.

Registration and observation grant no test credit. `restartWouldVoidEvidence` is
`true`, `false` or `null`, with a reason. Exact pending verification and a recorded
pass whose associated work lifecycle is unfinished remain `true` after the
process exits. `false` requires a positively validated unbound run, or settlement
of every associated check and canonical completion of each passing work lifecycle.
Missing or contradictory correlation is `null`; `true` dominates `null`, which
dominates `false`. Receipt ObjectIds are retained separately from the existing
canonical named-root read and its feed/read cut. Evaluation submission, release
and cancellation do not substitute for completion. Restart-loaded correlations
remain unknown until revalidated through existing producer boundaries. A terminal
unbound execution can be revalidated locally only from durable final host proof:
the original owner/runtime and turn still matched when it ended, work tracking was
positively disabled, and no binding, grant, check or report operation remained.
A registration or command-start snapshot and terminal artifact alone do not
supply that proof. The proof binds exact original execution and terminal facts;
changed facts or any later evidence association invalidate it. Known terminal
execution results survive boot independently of evidence safety. Historical
monitoring gaps remain visible without permanently poisoning revalidated proof.
Unresolved
terminal rows remain beyond the ordinary 50-row retention cap.

Resolved terminal detail uses one 50-row budget per project across all tracking
provenances, ordered by terminal time and stable identity; runs without a project
share an explicit separate bucket. Pending waits, reporting or persistence keep
their detail. Older resolved rows transfer every exact execution, registration
fingerprint, alias and verification association into a minimal durable record
before full detail is removed. Failed persistence keeps the original detail.
Launcher artifacts and Engram verification records are never deleted by this
policy. Rescans do not recreate resolved detail from unchanged old artifacts;
contradictory artifacts restore an unknown warning on the retained identity.
Unresolved discoveries retain their identity even when their artifacts disappear.

`GET /api/test-runs` returns one bounded page: `limit` defaults to 100 and
is capped at 200; zero, negative or malformed limits return HTTP 400. Optional
`projectId` filtering happens before pagination. Responses contain `runs`, the
effective `limit`, and `nextCursor` (null at the end). Pass a non-null cursor as
`after`, with the same project filter, to request the next page explicitly.
Malformed cursors or a changed project filter return HTTP 400. Cursors are opaque;
clients must not automatically drain the whole history.

Pages retain the existing order: `startedAt` descending (missing timestamps last),
then `runId` ascending. Continuation uses this stable key rather than an offset,
so new runs ahead of the cursor do not shift later pages. The list is live;
concurrent updates may move a run between pages. Refresh the first page for the
latest state. Exact-ID detail/log GETs remain independent of the page cap, as do
the live state snapshot and deltas that keep all active runs visible. Launcher
registration continues to query its exact run ID; it never lists the history.
The UI listing helper requests a single 100-row page and follows a continuation
only when its caller explicitly supplies one.

The list API discloses `detailHistoryLimit`, `compactedCount`, `omittedCount` and
`unresolvedBacklog`. Exact-ID detail reads distinguish retained compact identity
from a never-known run and explain when stage/log detail is unavailable. Minimal
resolved records stay out of recurring UI snapshots; bound proof becomes visible
as unknown after boot until existing canonical reads revalidate it. This bounds
resolved detail and normal payload/reparsing work, not total identity storage:
unresolved obligations and minimal immutable identity records remain lossless.

Bulk history projections (state snapshots, rescans and compaction selection)
build a temporary, borrowed index of artifact IDs to observation candidates.
The index lasts only for that locked read: every candidate still passes the
existing owner, directory, request/digest, durability and lifecycle checks.
It is not a cached verdict. A compaction completion revalidates its own exact
summary and proof rather than rebuilding all unrelated summaries. Idle ticks
still poll pending persistence fences, but skip history selection at a revision
already examined. Mutation paths that select directly and all rescans still do
so; runtime-only eligibility changes without selection or a revision bump may
retain detail until the next revision or the 10-second idle rescan. Failed
compaction/removal writes retain retry eligibility.

A lifecycle revalidation after a named-root read builds no full-history
projection while it holds the state lock. Each changed row publishes the
first run of its own execution id, or else of its artifact's run id and run
directory, selected from only those identities' summaries through one
borrowed candidate index per batch, so many changed rows cost one pass over
the retained rows, not one per row. This is the same first match the full
list gives; receipt settlement uses the same lookup. The revalidation does not select compaction
itself: it marks the examined revision stale, so the idle step (every 2 s)
selects at the new cut and compacts any run the revalidation made eligible.
Receipt settlement keeps its inline selection. The scan of retained
associations itself is still linear in history.

Host-observed native exits are execution observations. When the existing declared
artifact adapter judges a result, its verdict updates the row while retaining the
native exit code separately; exit zero cannot override a failing TRX. A restart or
lost provider runtime marks an unfinished observation unknown and records the
monitoring gap. The registry adds no process scanner or tracker poller.

Related: [Supervised Pilot Onboarding](supervised-pilot-onboarding.md), which
says what a pilot colleague sees of test runs and declared commands per tier.

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
1. the project captured by an exact, durable launcher registration, when present;
2. otherwise, the project whose root equals the run's worktree (`request.json` `root`);
3. otherwise, the project with the longest root containing that worktree;
4. otherwise, the project with the smallest id that shares the common directory.

Registration must match the request identity, owner and canonical native run
directory. Its host-captured locator digest is independent of display spelling
(macOS public/private aliases and Windows verbatim/UNC paths). A pending or
mismatched registration does not override discovery. If the registered project
was deleted, the surviving filesystem discovery determines the project. The notification recipient
only routes completion mail; it never changes the registered run's project.

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
  names, or that a card which has not yet stored its terminal snapshot names,
  as long as that card is in its session's in-memory window (a card outside
  it cannot be updated yet, see Card updates). So a long run that finishes
  behind 50 newer ones still settles its wait with its verdict, not as
  `UNKNOWN (not indexed)`.

### State

`state` is exactly one of:

- `passed` or `failed`: `results.json` is terminal. The value is copied from
  it. This includes runs settled by `recover`, which are `failed` with
  `interrupted: true`.
- `running`: `results.json` is readable and not terminal, and a fresh valid
  heartbeat or a responsible process that may be alive keeps it pending.
  Heartbeats use RFC 3339 `at` and integer `everyMs` in 1000..=600000. At the
  scan's observation time, at most one interval into the future is allowed;
  age at most three intervals is fresh. Freshness is reevaluated even when
  cached file bytes are unchanged. This is conservative pending evidence,
  not proof of an executor's identity. The repository launcher currently
  writes `creatorPid` and `pid`, not heartbeats; heartbeat support is reader-only.
  Without fresh evidence, the process
  check applies. That process is `results.json`'s `pid` when present,
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
  anywhere without a fresh heartbeat. A missing pid never proves death.
  A stale heartbeat without a pid reports `heartbeatStale`; absent or
  malformed heartbeat evidence without a pid reports `noPid`. Both remain
  pending uncertainty, visible on the card and in the wait status, with no
  recover advice. A launcher writes its terminal
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
`failed`. An `unknown` run has not been settled. `recover` may settle a
proven-gone executor but may refuse an unpublished or still-live executor;
stale or missing evidence alone does not authorize recovery. TermAl
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

The UI is Termal::Codex's, built against this contract. Its tracker items are
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
- **Sessions list busy state.** The Sessions list (docked and standalone)
  treats a session that owns a `running` run as busy even when its turn has
  ended (`ui/src/session-work-activity.ts`, from one by-owner index built in
  `ui/src/test-runs.ts`). Only the exact `ownerSessionId` counts: a
  notification recipient, an unowned run and a terminal or `unknown` run never
  make a session busy.
  - **Working** is the union of sessions with a foreground turn (`active` or
    `stopping`) and sessions with owned background work, each counted once;
    **Completed** leaves out an idle session with background work. Asking and
    Error keep their status buckets, so such a session can also be working.
  - The Working chip shows the split itself: "Working sessions (N) · F
    foreground / B background", or "Working sessions (N) (F/B)" when the
    Status section is narrower than 28rem. The compact form spells
    the split out in its tooltip. The chip's accessible description spells it
    out in either form, while its accessible name stays "Working sessions
    (N)". A session with both kinds of work counts in each, and with no
    working session the chip shows no split.
  - Each row says "Foreground turn", "Background work" or "Foreground turn +
    background work". During a turn, a run the turn started in the foreground
    (`detached: false`) is part of the turn; a host-observed run without a
    detach mode (the same rule as the marker's "test running") says "running
    work (mode unknown)", and the breakdown counts it as background. An idle
    row with background work shows a busy badge, not the idle one. In a
    narrow pane the label is cut with an ellipsis, and its full text is its
    title.
  - A run-only change (`testRunChanged`, `testRunRemoved` or a snapshot)
    updates the counts, filters and labels without a session update.
  - Only runs this index knows count: a detached build that was never
    registered as a run is not seen.
- The prototype linked on the tm-ncc6 epic is the visual reference.

## Slice 2: the test-run card

A test run is a specialized tool execution, so its card belongs to the
command-card family. It is the long-running execution view: an agent launches
the run detached and registers a wait. Its own command card finishes in
seconds with the launch receipt, and the test-run card that follows carries
the execution.

### When a card is created

- On its first boot with slice 2, TermAl stores `testRunCardsEpoch`, the
  current time, in persisted state. No card is created until that state is
  durable (see below). The epoch is never derived from "now" again.
- A card is created when the index sees a run that meets all of these:
  - its `ownerSessionId` resolves to a visible local session. TermAl sessions
    have no archived state (a Codex thread's `archived` is thread state), so
    this excludes internal sessions and, below, remote proxies;
  - its `request.json` `started` is at or after the epoch;
  - it is non-terminal at first sight, or it started at most 10 minutes
    before first sight. The second case catches a short focused run that
    ended between two rescans.
- "First sight" is the index's first observation of the run in this host
  process. So after a restart, a post-epoch run that ended while TermAl was
  down still gets its card if it started within 10 minutes of that boot's
  first sight. A longer one does not; it stays in the tab.
- A card is keyed by (owner session, runId). Persisted state holds a
  `testRunCards` map, from runId to `{ sessionId, messageId, terminal,
  failureDetailVersion? }`, the way delegation records point at their card
  message. `terminal` says whether the stored snapshot is terminal, so
  non-terminal cards are found without reading transcripts, and
  `failureDetailVersion` is the `detailVersion` the stored failure excerpt came
  from. The transcript stays the render source. Boot rescans, index churn and
  ageing-out never create a second card, and boot never scans transcripts to
  find cards.
  - An entry is dropped when its session or message is gone, and when its
    card is terminal and its run has left the index, unless the run started
    within the 10-minute window, where a returning run would still qualify
    for a card. So the map stays bounded by the index.
  - The map is written with the metadata, but a card's transcript row can
    reach disk one persist tick later. After a restart the transcript is the
    authority: each entry's `terminal` and `failureDetailVersion` are reset
    at load and re-derived by the first scan, which checks every entry and
    reads every failed carded run's excerpt again.
- The epoch must be durable before any card is created, so a restart can
  never store a later epoch under a card already shown. An epoch loaded from
  disk is durable. A new one is written through a persist fence that the
  rescan thread checks on each tick without waiting: until it resolves,
  rescans keep publishing runs and create no card, and a slow or failing
  persist never holds up the index. A failed fence is retried with the same
  epoch. On the scan after it becomes durable, every indexed run without a
  card is evaluated once, so a short run that started after the epoch and
  ended in that window still gets its card.
- A remote proxy's transcript belongs to its remote, so a run owned by one
  gets no local card.
- The card's `author` is `system`, a new author value: TermAl wrote it, not
  the user or the agent.
- Runs from before the epoch never get a card; they stay in the Test Runs tab.
- Only the owner gets a card. A `--notify` target alone gets none, because a
  notification is not a registered wait. A run started outside TermAl, with a
  null `owner`, gets none.
- The card is appended to the end of the owner's transcript when TermAl first
  sees the run, like a delegation card at spawn. It keeps its id and position
  and is updated in place.

### Rollout: the UI first

The served UI (`ui/dist`) is built and installed separately from the
backend, and a UI that predates the card does not handle it:
- its message height estimate has no case for `testRun` and no fallback, so a
  session holding a card likely gets a broken transcript layout;
- every `testRunCardUpdated` is an unknown delta to it, which forces a full
  state resync.

So a UI with the card renderer (tm-ncc6.13.3) must be built and installed
before TermAl restarts onto a backend that creates cards. A card UI works
with an older backend, which simply sends no cards.

The card backend landed switched off, and is switched on in source together
with the card UI (tm-ncc6.13.3), in one changeset. The switch is gone: the
rescan thread starts the card epoch on its first tick, the epoch is stored
before any card is created, and only runs started after it get cards.

Enabled in source is not deployed. Until the card UI is built and installed
into `ui/dist`, and then the host restarts onto this backend, the running
host keeps serving the old UI and creates no cards. The order stays the
same: install the UI first, then restart. A restart before the UI install
puts cards in front of a UI that cannot lay them out.

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
  tab. When the runId is not in the index, the tab shows an explicit view
  with the runId, the literal run directory, and the `summary` command with a
  `RUN_DIRECTORY` placeholder, since the path is not quoted into it.
  - Before the first state snapshot, the view says it is waiting for the
    index.
  - After it, the view says "Run not currently indexed". It does not claim the
    run was removed: discovery may still be in progress, and the wire has no
    flag for the first scan finishing.
- A card never shows `passed` without a terminal `results.json`. A proven-gone
  responsible process shows `unknown` with `processGone` and names `recover`
  as the way to inspect settlement. `noPid` and `heartbeatStale` instead name
  the unpublished executor and pending uncertainty, without recovery advice.
- There is no cancel control before slice 3.
- The card shows in the transcript only. The pane's Commands view lists
  `command` messages and leaves test-run cards out; the Test Runs tab is the
  list of runs.
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

type TestRunUnknownReason = "processGone" | "resultsUnreadable" | "noPid" | "heartbeatStale";
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
  - The version recorded is the digest of the bytes the excerpt was read
    from.
  - A read that fails (a replacement race, or an oversized file) is never
    recorded as "no failure": the card keeps its previous excerpt, and the
    read is retried on the next rescan even if the run's summary has not
    changed.
  - A read at a new version is recorded even when the card itself does not
    change, so it is not read again on every rescan.
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
- **The in-memory window.** A session keeps only the tail of its transcript in
  memory once idle (64 messages), and only that tail is written back. A card
  whose message has left that window is not updated yet: its map entry stays,
  and the card keeps its last snapshot. Updating such a card's stored row, and
  mirroring a remote's `testRunCardUpdated` into a proxied transcript (today
  the proxy consumes the revision without applying it), are the next
  changeset.
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
(see Refresh) keeps the last good results and any previously confirmed reason,
so that reason does not flicker. A failed read never establishes new proof of
disappearance. Renewed heartbeat or PID liveness clears an earlier
disappearance confirmation. When the bounded reads after checks
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

- **Caller.** Root sessions only, as for mailboxes: the tool is hidden from
  delegation children, and the backend rejects one (400). A child that ended
  its turn to wait would read as finished to its parent, then start a new
  turn when resumed. A child runs its gates in the foreground instead.
- **`runIds`.** One to 16 distinct ids (a repeated id is kept once), each
  indexed and in the caller's project. A caller without a project cannot
  wait on runs.
  - The caller need not be the owner: a coordinator may wait on a run it did
    not start.
  - Each new wait pins the request identity and a digest of the canonical native
    run directory, plus the frozen
    input fingerprint if launcher preflight has already published it. The
    request identity and fingerprint appear in the resume prompt. A replaced
    request, locator or
    pinned fingerprint cannot supply a PASS for that wait; it settles UNKNOWN
    because the original identity is no longer indexed. A matching completed
    run can be consumed through a new wait even if completion mail went to
    another session. Project isolation still applies.
  - If an id is not in the index, the tool first forces one rescan, waited on
    for at most 1 s; a longer one finishes in the background and the call
    proceeds with the index as it is. Only then does an unknown id (404) or a
    foreign one (400) reject the whole call. So an agent that registers right
    after launching a detached run is never pushed into polling.
  - Rescans are serialized. The tool's forced rescan and the background
    rescan take the same rescan lock, so a scan that started earlier never
    commits over a newer one.
  - Each registration that names an unknown id starts one rescan thread, and
    nothing coalesces them. A burst of such registrations behind a long
    rescan queues that many threads on the rescan lock, and each then scans
    in turn. The bound is the number of those registrations: agent tool
    calls, a handful at a time.
  - The wait's mandatory prompt content, meaning every run's header and
    commands, must fit in 48 KiB. A registration that would exceed that is
    rejected, which leaves at least 16 KiB for excerpts.
- **`mode`.** Defaults to `all`.
- **Result.** `{ waitId, runIds, mode }`, plus the record (`wait`), the
  revision, and whether a resume was queued (`resumePromptQueued`) and
  dispatched (`resumeDispatchRequested`) at once.
- **HTTP.** The tool posts to `POST /api/sessions/{id}/test-run-waits`, where
  `{id}` is the waiting session; it answers `201`.
- **Already settled.** A wait on runs that have already settled queues its
  resume at once and never blocks. This is also how a session re-fetches a
  verdict.
- **No mixed waits.** A wait cannot mix delegations and runs.

### Settled

A run is settled for a wait when it is `passed` or `failed`, or `unknown` with
`unknownReason` of `processGone`, confirmed by the post-liveness reread.

A run that left the index after the wait was registered settles as `UNKNOWN
(not indexed)` when its original identity cannot come back:

- its `request.json` is gone (with its directory or alone) or cannot be read,
  checked on disk without the state lock; or
- the waiting session's project no longer exists. After `delete_project`
  that is also the run's project, since a wait requires them to match. The
  session stays, without a project, and the index stops scanning that
  repository.

For a fenced wait, a readable replacement also settles UNKNOWN: a changed
run ID, request digest, canonical native locator or pinned fingerprint, or
an unsupported request, cannot stand in for the original. A same-ID index
entry with a mismatched identity likewise settles UNKNOWN.

A matching pinned original that is still readable and supported remains
pending for reindexing. Unfenced legacy labels retain the readable-request
rule. A rescan that started before registration may lack the wait's retention
exemption and drop a terminal run past the per-project cap; the next scan
indexes the matching original again, and it settles with its real verdict.

An `unknown` run with `noPid`, `heartbeatStale`, `resultsUnreadable`, or with
no reason is not settled: the run may still finish, and the index retries.
The pending wait status uses the same uncertainty reason as the card; an
unpublished executor is never described as proven gone.

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
  - The refresh runs after every background rescan, once the rescan has
    released its lock (resuming may dispatch a turn), and after each
    registration. Failure excerpts are read from disk without the state
    lock.
  - A scan can land between picking the excerpts to read and queuing the
    resumes. A wait whose run failed in that window has no read yet, so it
    stays pending until the next refresh reads it; it never resumes without
    diagnostics that are on disk. A read that was made is an answer and does
    not defer the wait, even one that found nothing or could not read
    results.json whole (a replacement race, or a file over the 1 MiB read
    limit): the prompt then says the diagnostics are not available here.
- **Stop or archive.** A Stop or an archive consumes the session's pending
  run waits. Runs and cards stay, and nothing reactivates on its own. An
  archived session gets no new card and no wait resume.
  - A Stop consumes them as part of its durable commit (reason
    `sessionStopped`) and restores them if that commit fails, as for
    delegation waits.
  - A session that ended its turn to wait is idle. Stop there has no turn
    to interrupt, but it is still the brake on the resume: it consumes the
    session's run waits (`sessionStopped`) and delegation waits
    (`parentSessionStopped`) in one durable commit, sets the
    explicit-resume latch, drops queued workflow continuations, keeps user
    and mailbox prompts queued behind the latch, and records "Turn stopped
    by user." A failed commit restores all of it. An idle session with no
    pending wait still answers 409. An explicit Resume or a new user prompt
    lifts the latch. The session view offers Stop in this state.
  - A session that became an archived Codex thread (`sessionUnavailable`) or
    was removed (`sessionRemoved`) has its waits consumed by the next
    refresh, within one rescan. A thread archived and unarchived again
    before that refresh keeps its waits, as delegation waits do. Consuming
    them at the archive itself is tm-9tbr.
- **Restart.** Pending waits are reloaded. After the first index scan, a wait
  whose runs have settled resumes once, and only for an idle session that is
  not latched. No wait settles before this process's first full scan, since
  an empty index would read every run as not indexed.

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
  requestIdentity?: string;     // immutable request digest
  expectedFingerprint?: string;// frozen input if published before the wait
  locatorIdentity?: string;     // SHA-256 of canonical native path bytes,
                                // never display text
};

type TestRunWaitConsumedReason =
  | "completed"
  | "sessionStopped"
  | "sessionUnavailable"
  | "sessionRemoved";

type TestRunWaitCreatedEvent = {
  type: "testRunWaitCreated"; revision: number; serverInstanceId: string;
  wait: TestRunWaitRecord;
};
type TestRunWaitConsumedEvent = {
  type: "testRunWaitConsumed"; revision: number; waitId: string;
  serverInstanceId: string; sessionId: string; reason: TestRunWaitConsumedReason;
};
type TestRunWaitResumeDispatchFailedEvent = {
  type: "testRunWaitResumeDispatchFailed"; revision: number;
  sessionId: string; error: string;
  serverInstanceId: string; // absent from backends before tm-ncc6.8.4
};
// The backend decodes a remote's event without serverInstanceId as an empty
// string, so a remote on an older build does not break remote delta decoding.
```

`StateResponse.testRunWaits?: TestRunWaitRecord[]` lists pending waits only,
and is omitted when empty. There is no "changed" event: a wait's fields never
change after creation.

### Resume prompt

There is one prompt per wait, never repeated log or card messages. Its overall
cap is 64 KiB. For each run it contains:

- a header: runId, preset, verdict (`PASS`, `FAIL` or `UNKNOWN (reason)`),
  `interrupted`, exit code, start and end times, and the run directory;
- for `FAIL`, the first failing stage or preflight check with its
  diagnostics excerpt (`First failing: stage NAME` or `First failing:
  preflight NAME`). Both come from reading results.json, which names a
  preflight check or a stage past the summary's 64; the summary's stages are
  the fallback when the read names none. For `UNKNOWN`, the stage that was
  running at the last observation;
- the commands to inspect it: `node scripts/test-launcher.mjs summary
  RUN_DIR`, plus `recover RUN_DIR` for `UNKNOWN`.

Headers and commands are never cut; registration bounds them to 48 KiB. Each
run's excerpt budget is min(8 KiB, (64 KiB minus all headers and commands) /
number of runs), and never negative. `UNKNOWN` is never phrased as a pass.
In an `any` wait, a run that has not settled yet is listed as `NOT SETTLED
YET` with the stage at the last observation. A failed run whose diagnostics
cannot be read at resume time says so and points to the summary command.

An excerpt is marked `(cut)` when the prompt's budget cut it, and also when
it was cut before: the launcher caps diagnostics and records that it did, and
the read caps them at 8 KiB. Its code fence is longer than any run of
backticks in the excerpt, so test output that contains a fence cannot close
it.

### Rollout: ships switched on

Run waits add no `Message` variant, so unlike the card (see "Rollout: the UI
first") they ship switched on, with any UI. What a UI without run-wait
support does with them, traced in `ui/src` at 39977e5:

- **`testRunWaits` in the state response.** Ignored: the UI reads the state
  response by field name, with no validator.
- **`testRunWaitCreated`.** It has no top-level `sessionId`, so the session
  reducer's exhaustive default answers `needsResync`: one `/api/state`
  resync.
- **`testRunWaitConsumed` and `testRunWaitResumeDispatchFailed`.** They carry
  a top-level `sessionId`, so the UI takes them for session deltas. The same
  default answers `needsResync`, which triggers a `/api/state` resync and a
  forced reload of that session's recent transcript
  (`triggerRecoveryForDelta`), deduplicated by the hydration queue.

That is one or two bounded reloads per wait created or consumed. The resume
prompt reaches the transcript as an ordinary queued prompt.

The UI that ships with the card UI (tm-ncc6.8) handles all three events and
never takes them for session deltas, so it never reloads a transcript for
them:

- **`testRunWaitCreated` and `testRunWaitConsumed`** are handled like
  delegation wait deltas. Each carries only part of its commit: a consume also
  queued a resume prompt, and one commit can consume several waits at the
  same revision. So the UI updates the list of pending waits by wait id,
  never advances the global revision from these deltas, and asks for an
  authoritative `/api/state` repair of that revision. The repair fetches are
  coalesced (one in flight). A created wait older than the adopted state, or
  than the newest wait delta applied, is ignored, so a late create cannot
  bring back a consumed wait; a consume removes by id at any revision of the
  adopted server. Both wait families carry `serverInstanceId`: retired-server
  deltas are discarded before local updates or repair bookkeeping. Missing or
  unseen identity requests a snapshot without applying the delta or comparing
  its revision to the current server. An unseen nonempty identity enables the
  authoritative replacement recovery for that identity, allowing that snapshot
  to prove the new server even at a lower revision; missing identity grants no such
  permission. Missing identity can still trigger an equal-revision repair from
  the same known server: a session-only response may have advanced the global
  revision without refreshing waits. It cannot permit a revision downgrade.
  Fetch-failure retries retain those restrictions; a failed request alone is
  not evidence of a replacement server. A separate timer retries the projection
  repair until an acceptable snapshot is adopted, with one fetch in flight and
  capped backoff. This includes tagged probes rejected after in-flight progress,
  and stale current-server creates after a session-only response. Unrelated SSE
  traffic cannot cancel it. Retries retain only wait-specific evidence: a tagged
  replacement hint remains valid only while its observed origin is still current.
  One-shot live-connection proof, navigation and generic replacement permissions
  from coalesced requests are not replayed. Reconnect polling remains independent.
  Missing identity handling accommodates a version-skewed development UI; current
  local producers always tag wait events and remote wait events are not forwarded.
  Each list retains its newest delta's
  revision and skips replacement by older snapshots; a snapshot of the same
  revision covers sibling changes. These watermarks are scoped to the server
  and reset when an accepted snapshot changes that identity to a nonempty one.
- **`testRunWaitResumeDispatchFailed`** is routed before that gate and never
  advances the revision. It reports an outcome after the wait was consumed,
  not a state change, and can carry the same revision as the consume or an
  older one. The UI keeps the latest error per session: a repeat of the same
  revision and error, or an older revision, from the same server instance does
  not replace it. Revisions are not compared across instances, because a
  restart can roll the revision back. Failures from previously adopted,
  retired servers are rejected; an unseen server's early failure is accepted.
- **The error notice** shows in the session pane's toolbar and on the board,
  apart from connection errors. It stays through unrelated deltas, snapshots
  and tab changes, until the user dismisses it, the session is removed, or
  the UI adopts a snapshot from a different server and the failure did not
  come from that server (see "Server identity"). A dismissed error does not
  come back when the same event repeats. The notice lives in the browser
  session only, so a reload drops it; the backend's `test run wait warning>`
  line on stderr is the lasting record. Opened in the pane toolbar, its error
  text (at most 12rem tall) grows the toolbar and moves the transcript down by
  that height; this is accepted. Only the pane's notice carries a live region;
  the board's copy is a plain label, so two mounted notices do not both
  announce. The region mounts with its text already in place, which screen
  readers do not always announce.
- **Server identity.** The event carries the `serverInstanceId` of the server
  that failed to dispatch. When the UI adopts a snapshot from a different
  server, it keeps the failures tagged with that new server and drops the
  rest, including untagged ones from older backends. A failure that a
  restarted server sends before the UI has adopted its first snapshot
  therefore survives the switch, instead of the session looking plainly idle.

### Waiting indicator

The indicator sits in the session pane's waiting indicator, next to delegation
waits, and on the board. It is not a transcript message, but the transcript's
reserved activity line shows "Agent is working" for it, as it does for a
delegation wait. For example: "Waiting for all test runs: RUN (full, running,
stage rust-tests, 3m12s) — TITLE". A wait in `any` mode says "any test run",
and a run that has left the index shows "not indexed" and no elapsed time.

- It shows only while a wait is registered and the session is idle. It is
  never inferred from a running card, and it hides while the session is
  actively responding.
- The live-region announcement leaves out the elapsed time, so screen readers
  are not re-told every second. The visible text and its tooltip keep it
  current.
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
- [Host Architecture](./host-architecture.md): the proposed execution record,
  in which a launcher run is one kind of execution.
- [Handoff Ledger](./handoffs.md): a recorded next step the host offers its
  owner when a run ends or is credited, with no wake needed.
- [Remote Full Gates](./remote-full-gates.md): the five-stage gate run on a
  satellite and reported back to the central coordinator.
- [Satellite Onboarding](./satellite-onboarding.md): adding the satellite a
  remote gate runs on.
- [Project Status](./project-status.md): the latest remote gate results and
  the open problems.
