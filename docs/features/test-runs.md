# Test Runs

TermAl shows the test runs its agents start: which runs are active, their stages
and timings, their logs, and which sessions own each run or will be notified
when it finishes. Later slices let TermAl start, supervise and cancel runs
itself, and wake the requesting session when a run completes.

The runner is `scripts/test-launcher.mjs` (see [Testing](../test.md)). Its run
directories are the record of truth; TermAl mirrors them and never decides a
verdict itself.

**Status: slice 1 approved by Greg (2026-09-25) and in implementation.** Slices
2 and 3 wait on the two owner decisions marked **Decision** below.

## Slices

1. **Visibility (read-only).** Index the launcher's run directories. Publish
   run summaries in the state snapshot and by live delta. Serve run detail and a
   bounded stage-log tail. Show a Test Runs tab and session markers. This covers
   runs started by agents through the CLI, including detached runs with
   `--notify`.
2. **Host-owned runs.** TermAl starts the foreground launcher as a hidden,
   supervised child. It then offers cancel, rerun, one run per worktree, live
   log follow (SSE) and boot reconciliation.
3. **Agent tools and wake.** `termal_start_test_run`, `termal_get_test_run`,
   `termal_list_test_runs` and `termal_cancel_test_run`, plus registered run
   waits. Completion is a durable activation of the requesting session, so the
   requester may wait on its own run.

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
  project.

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
  anywhere. A missing pid is never `running`.

`interrupted: true` is copied from `results.json` and only ever appears with
`failed`. An `unknown` run has not been settled; `recover` settles it. TermAl
never writes to a run directory in slice 1.

### Refresh

A host thread rescans the run directories: every 2 s while any indexed run is
`running`, and every 10 s otherwise, so an abandoned `unknown` run does not
hold the fast rescan until someone recovers it. TermAl's workspace file watcher
deliberately ignores `.git/`, where the runs live.

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
run id, `owner` or `notifyTo` exceeds 128 bytes, or whose worktree exceeds 4096
bytes, is not indexed. A stage whose name is not a launcher stage name
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
- Every route is read-only in slice 1. Live follow (SSE) comes in slice 2.

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
     idle or waiting; registered waits come in slice 3.

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

## Later slices: decisions and contract

- **Decision (restart survival).** Proposed: option B. Test runs get a
  dedicated job object without kill-on-close. A TermAl restart then leaves the
  run alive, and TermAl re-attaches it by pid plus process creation time.
  Cancel still terminates the job. The alternative, option A, keeps
  kill-on-close: every TermAl restart then makes the run `unknown`.
- **Decision (Stop).** Proposed: option (i). A user Stop consumes pending
  run waits, as it does delegation waits. The run and its result stay visible
  in the tab, and nothing reactivates automatically.
- **Admission.** One run per worktree. That means no non-terminal host record
  for the root, and no indexed `running` run.
- **Cancel.** Refused (409) for runs TermAl did not start.
- **Rerun.** Rebuilds the CLI invocation from `request.json` and creates a new
  run. It is refused (409) while a run is active for the worktree.
- **Remote.** Starting, cancelling or rerunning a run for a remote project
  returns 501.
- **Wake body.** A host header (state, reason, run dir, fingerprint, drift)
  plus the launcher's `summarize()` text. The overall cap is 64 KiB, and log
  bodies are never included.
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
  the model for the slice 3 wake.
- [Durable Agent Mailboxes](./agent-mailboxes.md): `--notify` delivery for
  coordinators outside TermAl.
- [Work Visualizer](./work-visualizer.md): the tab pattern the Test Runs tab
  follows.
