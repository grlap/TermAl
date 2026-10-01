# Test Plan

This document describes the current TermAl test strategy and the commands used
before reviews or releases.

## Current State

TermAl has both backend and frontend automated tests.

Backend:

- Rust unit and route tests live in `src/tests.rs`.
- Tests cover state persistence, remote routing, SSE parsing, terminal command
  execution, project deletion, workspace layouts, file/git APIs, agent-runtime
  normalization, and regression edges.
- Run with `scripts/test-rust.sh`.

Frontend:

- Vitest + React Testing Library are configured in the Vite app.
- Test files live next to the TypeScript modules they exercise, using
  `*.test.ts` and `*.test.tsx`.
- Coverage includes workspace state, tab drag/drop, live updates, session
  reconciliation, model options, slash-command behavior, message cards,
  source/diff/file/git panels, terminal panel behavior, remotes, themes, and
  the main `App` integration harness.
- Run with `cd ui && npx vitest run`.
- Treat nondeterminism as a defect in the test, its assumptions, the runner
  resource contract, or the product. Do not use retries, quarantine, or larger
  timeouts as a substitute for finding and fixing the cause.

Type/build checks:

- Backend compile check: `cargo check`.
- Frontend type check: `cd ui && npx tsc --noEmit`.
- Frontend production build: `cd ui && npm run build`.

## Review Gate

The maintained full gate is one launcher invocation:

```bash
node scripts/test-launcher.mjs full
```

It runs, in fail-fast order, `cargo check`, TypeScript `--noEmit`, the review
fingerprint and launcher fixtures, the native Git Bash Rust wrapper, and the
full Vitest suite. Fingerprinting, Cargo and Rust stay rooted at the repository;
TypeScript and Vitest execute with `ui/` as their real working directory so
relative source and fixture paths retain the same semantics as `cd ui`. On
every platform, the effective Cargo is `TERMAL_TEST_CARGO` when explicitly set,
otherwise `cargo` resolved from `PATH`; that same executable is used by the
compile check and passed to `scripts/test-rust.sh`. On Windows, TypeScript and
Vitest use their JavaScript entrypoints rather than `.cmd` shims. All commands,
working directories and required files are preflighted, and a missing
prerequisite leaves every stage explicitly unrun.

For an authorized focused check, pass an argument array after `--`:

```bash
node scripts/test-launcher.mjs focused -- node --test scripts/test-launcher.test.mjs
```

Name a shell explicitly when one is required. On Windows, direct `.cmd` and
`.bat` executables are rejected; use Node with the package's JavaScript CLI or
an explicit known native shell. No launcher preset installs dependencies,
builds the production UI, touches `ui/dist`, restarts a host, or accesses live
store policy.

Each run owns a unique directory below Git's `review-runs` metadata directory.
A foreground run prints `RUN RUN_DIRECTORY` before any stage executes, so an
interrupted host wait can still inspect or settle the run without rerunning it.
`request.json` records the exact plan and captured source fingerprint, plus
`detached` and the creating process's `creatorPid`, which TermAl's test-run
index reads (see [test runs](features/test-runs.md)); `results.json` records
actual process exits, explicit unrun stages, timestamps,
and full log paths. Terminal JSON replacement is atomic for concurrent readers
on the same filesystem; it is not a claim of power-loss or crash durability.
Diagnostic extraction is bounded and does not decide success. A missing
terminal result is `UNKNOWN`, never a pass. Source/index drift before or during
execution invalidates the run, and `execution.lock` prevents rerunning the same
plan.

The fingerprint's own Git calls ignore system and global Git configuration
(`GIT_CONFIG_NOSYSTEM`, and `GIT_CONFIG_GLOBAL` pointing at the null device), so
the same working tree always hashes the same way whatever the machine's Git
settings. Two consequences follow. A file ignored only through a global
`core.excludesFile` still counts as untracked source, so creating or changing it
during a run is drift. And the recorded index and working-tree state are those
the isolated Git reports, which can differ from an ordinary `git status` where
system settings such as `core.autocrlf` apply. A drift error names the changed
fingerprint components, not the paths within them.

An existing root worker may deliver completion to a different coordinator:

```bash
node scripts/test-launcher.mjs full --detach --notify COORDINATOR_SESSION_ID
```

The worker must inherit its genuine `TERMAL_SESSION_ID`, absolute `TERMAL_CLI`,
and host connection environment. Self-send and identity changes are rejected.
After the `STARTED` receipt, end the turn and wait for the genuine mailbox wake;
do not poll status, tail logs, or launch a watcher. Completion is saved before
notification. Recover without rerunning tests with:

```bash
node scripts/test-launcher.mjs summary RUN_DIRECTORY
node scripts/test-launcher.mjs notify RUN_DIRECTORY
```

`notify` reuses the saved message and stable idempotency key. It never executes
the test stages again.

If the worker dies after `STARTED`, no completion arrives. Nothing watches for
that. Settle such a run on request with:

```bash
node scripts/test-launcher.mjs recover RUN_DIRECTORY
```

`recover` refuses while the launcher pid recorded in `results.json` may still
be alive (only proof of its exit counts), and refuses a run no process ever took
ownership of, since no stage ran under it. Otherwise it records the run as
failed and interrupted: a stage that was running gets an unknown outcome, never
a pass, and no stage runs again. The result is read again under
`recovery.lock`, so a terminal result the worker saved in the meantime is kept,
and the lock is released after every attempt, so a failed write can simply be
retried. It then sends the completion under the run's own key, but only when run
by the session that owns the run; a coordinator may settle and read the run but
not speak for its owner. A second `recover` changes nothing and never resends a
delivered completion.
[Host architecture](features/host-architecture.md#84-launcher-recovery-is-a-declaration-about-a-run)
describes how a recovered run is recorded in the proposed execution record.

Only the launcher's own pid is checked. A stage process that outlived a killed
foreground launcher is not waited for: its exit status and diagnostics are never
recorded, but it keeps its log open and can go on appending to it until it
exits, so a settled run's raw log is not necessarily final.
If the recorded pid has been reused by an unrelated process, `recover` keeps
refusing and the run stays `UNKNOWN`, which is never a pass: start a new run
instead of editing its evidence. If a killed `recover` left `recovery.lock`
behind, remove that file by hand once no other `recover` is running.

When a command is launched through TermAl's native Windows terminal or bounded
reader with confirmed job membership, the host owns a kill-on-close job and
ends its members after observing root exit. An `unavailable` launch can run
without that job. The standalone launcher and `recover` do not provide that ownership.
See [Windows command containment](features/windows-command-containment.md) for
the reported status, packaged-process limits and retained-handle fixture proof.

If an admitted worker cannot save its terminal result, for example because
`results.json` cannot be replaced, it sends one best-effort `UNKNOWN` notice
under the separate key `termal-tests:RUN_ID:runner-error`. That notice points to
`summary` and `recover`, and leaves the run's own completion key unused for the
settled result. While `results.json` itself cannot be written, `recover` fails
the same way and the run stays `UNKNOWN`. If the terminal result was already
saved and only a later step failed, the worker sends the run's normal
completion instead. Neither `notify` nor `recover` sends a completion for a run
whose request failed validation.

When a run fails, `results.json`, the compact summary, and any mailbox
completion contain an `INVESTIGATION REQUIRED` handoff. That handoff does not
claim the launcher can diagnose arbitrary code; it makes the agent-owned next
step explicit.

A failed gate is an investigation trigger, not permission to retry until green.
Preserve the original run and logs, state falsifiable hypotheses, use focused
discriminating diagnostics, and classify the cause as product, test/runner, or
environment/resource. Fix confirmed in-scope test or runner defects without
another user approval round trip. A later pass is validation of the fix; it is
not by itself a diagnosis or closure of the original failure. Never automate
retry/repair/reviewer loops, weaken or ignore tests, inflate timeouts, or change
product semantics to obtain green. Escalate when the evidence requires product
behavior changes, destructive or external actions, missing authority, or a
genuine blocker.

The operator-run disposable Engram suite requires an absolute binary and its
reviewed SHA-256:

```bash
node scripts/test-launcher.mjs live --engram-binary C:/absolute/engram.exe --engram-sha256 SHA256
```

The binary path, fingerprint, executable probe, native Cargo and Git Bash are
verified before the ignored live stage starts. A mismatch is a terminal failed
run with the stage unrun; required live checks are never silently skipped.

The Rust wrapper raises the inherited Unix file-descriptor soft limit toward
4096 and defaults libtest to four threads. This prevents FD-heavy SQLite,
HTTP, and runtime fixtures from intermittently exhausting macOS's common
256-descriptor default. Set `TERMAL_TEST_FD_LIMIT` or `TERMAL_TEST_THREADS` to
positive integers to override those defaults; existing `RUST_TEST_THREADS` is
also honored when `TERMAL_TEST_THREADS` is unset. On WSL, a checkout under a
mounted Windows drive uses `cargo.exe` by default so the gate matches the
Windows application's path semantics; the diagnostic names the selected Cargo
and notes that the Unix descriptor limit cannot apply to that Win32 process.
Set `TERMAL_TEST_CARGO` to an executable name or path to force a different
toolchain. Extra arguments are passed through to `cargo test`, for example:

```bash
scripts/test-rust.sh mailbox_store_tests
```

The wrapper requires Node.js (the version in `.nvmrc`) on `PATH`, including
`node.exe` when WSL launches Windows Cargo. Node owns the temporary run directory,
redirects `TMP`/`TEMP`/`TMPDIR` into the product's `termal/tests/run-*` folder,
reports newly escaped artifacts, and removes successful runs without retries.
A failed run is retained for diagnosis; removal failures name the path, OS error
and surviving entries. Stale marked runs are swept only after the recorded
processes have exited and the age threshold has passed.

Direct `cargo test` and `cargo check` do not require Node. Direct tests use the
Rust product-temp helper where adopted, but bypass the wrapper's environment
containment, end-of-run audit and marked-run sweep; use the wrapper for the full
gate while raw temp call sites are still being converted. Both Node and Rust
helpers require absolute paths without `..` components. A manually supplied
`TERMAL_TEST_RUN_ROOT` must be a direct `run-*` child of the product tests folder.
Paths are normalized lexically, not canonicalized through filesystem aliases;
product and run directory components must not be symlinks or junctions.

The review-integrity helper tests run on Linux, macOS, and Windows in CI: the
same four maintained suites as the full gate's `fingerprint-tests` stage
(`helperTestFiles` in `scripts/test-launcher.mjs`), which a launcher test keeps
in step with the workflow. The
Vitest resource preflight itself uses three fixed CPU samples and the median,
so one scheduler spike does not reject a gate while sustained starvation still
fails before frontend tests start. Windows reports process CPU availability
without presenting its unsupported load-average value as real system load.

### Credit for a background or detached gate

A full gate takes longer than a Claude tool call may run (10 minutes), so a
session that needs the gate as evidence for an Engram item it holds launches
it in the background, in the item's named source root:

```bash
pushd "C:\path\to\worktree" && node scripts/test-launcher.mjs full
```

with the Bash tool's background mode, then ends the turn. A runtime that can
notify a coordinator may launch it with `--detach` instead. Only a root
session whose turn is measured in the item's named source root gets credit
this way; a delegated session is told its gate is not carried. The gate goes
alone on its line: a launch joined to another command, or a `--detach`
launch that failed, earns no credit and TermAl says so; relaunch two minutes
or more later, since a launch that follows an unmatched one sooner is
refused as ambiguous. TermAl carries the
launch past the turn and credits the run as a passed test check on the
holder's next checkpoint for the same claim, when every stage the run
requested passed, among them a test stage ([Which stages count as
tests](#which-stages-count-as-tests)), its input
fingerprints before and after are present and agree, its terminal record is
the one TermAl first read as terminal, the worktree's source is unchanged,
and no write TermAl can observe reached the worktree before the run ended.
A failed run with a complete record is recorded as failed; a run that was
stopped or interrupted, or whose launcher died without a result, is
neither, and TermAl records nothing for it. While it runs, a command the
holding session runs in that worktree refuses the credit unless the session
is a Claude session and the command only reads (such as `git status`,
`git diff`, or `node scripts/test-launcher.mjs summary RUN_DIRECTORY` from
the repository root). A Claude session's background launch leaves its shell
where it was, because the Bash tool runs a background call in a shell of its
own: a later command with no `pushd` of its own runs where the session's
shell already was, not in the gate's worktree. The credit is also refused
by a command another writable session runs in that worktree, unless that
session is a Claude session and the command only reads (a command TermAl
cannot place counts where its session works: its workdir's worktree and its
claim's named source root, not every worktree on the host); by a file edit
reported by another session that works there; by any
file edit the holding session reports; and by any file change TermAl's
workspace watcher sees there, except under the worktree's own `.tmp/`
scratch directory. Once the gate is carried, another session being in a turn
refuses nothing by itself. At the launch it does: a gate launched while
another writable session is in a turn in that worktree, or while another
command of the holder is running, is not carried at all. TermAl says which
and why before the next prompt. The launcher makes Cargo's `target`
directory itself before the first stage, when Git ignores it and no
`CARGO_TARGET_DIR` points elsewhere: Cargo would otherwise make it through
a temporary sibling (`target` plus random characters) that the watcher does
not ignore, and the first build in a fresh worktree would refuse its own
gate. A command run through Claude's PowerShell tool is not reported
to TermAl, so only the watcher can see what it writes. Once the run has
ended, a write no longer refuses it, except a change the watcher reports
late, which may have landed while it ran; a source change still there at the
checkpoint does. A host restart during the run loses the credit, and TermAl
says so after it. An acceptance evaluation requested before the record lands
is judged without it, and Engram marks it stale when the record lands (a
check recorded after an evaluation's evidence basis makes it stale), so
request it after the checkpoint that records the gate. The full rules are in
[the Engram host adapter's carried background gates](features/engram-host-adapter.md#carried-background-gates).

### Which stages count as tests

Another project can run its own copy of the test launcher, with its own
stages. The rule, agreed with the Engram project's coordinator, applies to a
full gate in the foreground and carried alike:

A full-gate stage counts as a test stage when the run's request record gives
it `kind` `test`. When no stage in the request record has a `kind`, the
stages named `rust-tests` and `vitest` count instead. A run whose request
record gives kinds but marks no stage `test` is not credited. A full gate is
credited as a passed test check only when every requested stage passed with
exit 0 and at least one test stage is among them. A full gate that failed,
and whose request record has a test stage, is recorded as a failed test
check for that run, whichever stage failed, so it stays visible as the
newest check. In the foreground the host reads the request record from the
run directory the launcher names; a carried run is judged from the request
record the host already reads.

TermAl's own launcher writes no kinds, so its `rust-tests` and `vitest`
stages are its test stages. Engram's writes a kind for every stage, and its
`rust`, `freeze`, `mcp`, `control` and `parity` stages are `test`. In the
foreground, the launcher names its run directory in its summary's
`results: PATH` line: the request record is the `request.json` beside that
`results.json`. A summary with no such line gets TermAl's two names. A
failed full gate whose request record has no test stage is still recorded
as failed, as every failed gate was before this rule: a failure can only add
a block, never let an older pass stand. A focused run is outside this rule.

## Backend Testing Guidelines

Prefer focused Rust tests in `src/tests.rs` for:

- pure parsers and normalizers
- path validation and canonicalization
- persistence projections
- API route behavior through `tower::ServiceExt`
- remote proxy edge cases
- terminal stream framing, cancellation, truncation, and 429 behavior

Backend tests should avoid starting real agents. Use test HTTP listeners,
temporary directories, injected remote configs, and helper state builders
instead.

Some tests expect different text depending on the test directory's path,
and each branch asserts its exact text: the Engram one-call tests
(`src/tests/engram_one_call_checks.rs`) offer the form `pushd "DIR" && TEST`
only for a path the form can name, and advise a `cd` only into a path TermAl
follows one into. Each such test runs once per kind of path, set by a suffix
of its fixture's label, which becomes part of the project's path: plain,
` (x)` (the form names nothing there) and ` $x` (TermAl follows no `cd`
there). A default run takes the ` (x)` and ` $x` branches on any host, as
`the_unreadable_path_kinds_reach_their_branches_on_any_host` checks. The plain kind can
only add to the host's temp path, so it takes the branch where the form can
name the path, which credits a one-call line end to end, only where that
temp path, as resolved, is one the form can name: ASCII letters, digits,
spaces and `_ . - /`, as a default temp directory usually is. On a host
whose temp path is not (a non-ASCII user name, for example), a default run
passes without that branch; to run it there, point the product test temp at
a plain directory inside the repository, given as a native absolute path:

```bash
mkdir -p ".tmp/plain"
TERMAL_TEST_USER_TEMP="$(node -p "require('path').resolve('.tmp/plain')")" \
  node scripts/test-launcher.mjs focused -- cargo test --bin termal one_call
```

The repository's own path must then be plain too. The suite does not
require a plain path, since a precondition on the host would make it a
lottery.

## Frontend Testing Guidelines

Prefer pure TypeScript tests for reducers and helpers:

- workspace tree/tab operations
- path display and validation
- state revision adoption
- live delta application
- session model option normalization
- remote config normalization

Use React Testing Library when the regression depends on rendered behavior:

- keyboard navigation
- combobox selection
- focus management
- scroll behavior
- panel-specific user flows
- stale file and conflict recovery actions
- terminal streaming UI

Keep integration tests focused. The main `App.test.tsx` harness is valuable but
expensive; prefer extracting pure helpers or testing a panel directly when that
captures the bug.

## Known Coverage Gaps

The active follow-up list lives in Beads. Use `bd ready` for currently
unblocked work and `bd list --status=open` for the wider open inventory.
Coverage and type-surface improvements are tracked there alongside their
priority and dependencies.

Current gaps:

- multi-commit session scroll pinning needs a render-level regression test
- one settled-scroll integration test should assert the explicit `minAttempts`
  floor more directly
- the exported `isScrollContainerAtBottom` helper needs either deletion or a
  comment explaining why the dead export is intentional
- `setAppTestHooksForTests` should either be tree-shaken from production or
  documented as non-sensitive test-only surface
- `AppTestHooks` should be exported for cleaner test typing
- `resolveSettledScrollMinimumAttempts(0)` needs a small edge assertion

## What Not To Test

- Do not test full external agent CLIs in normal unit suites.
- Do not depend on a user's real `~/.termal` directory.
- Do not require network access for default tests.
- Do not make frontend tests depend on real Monaco layout measurements unless
  the test is explicitly about Monaco integration.
- Do not add broad snapshots of the whole app; assert the behavioral contract
  being protected.
