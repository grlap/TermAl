# Test Plan

This document describes the current TermAl test strategy and the commands used
before reviews or releases.

The [durability-pending contract](features/durability-pending.md) describes
post-enqueue registration deadlines: pending is not stage-launch permission,
and an explicit retry keeps the original request and run identity.

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
- Activity indicators have a real-browser guard:
  `node scripts/check-activity-indicators.mjs --chrome-executable ABSOLUTE_PATH`
  from the repository root after `npm ci` in `ui/`. It checks working/stopping
  labels, reserved geometry, 30-frame-per-second indicator/dot motion and
  synchronized starts for one and six sessions. Offscreen/background motion
  pauses, hidden/reduced-motion cases have no timelines, and visible motion
  resumes after backgrounding.
- Treat nondeterminism as a defect in the test, its assumptions, the runner
  resource contract, or the product. Do not use retries, quarantine, or larger
  timeouts as a substitute for finding and fixing the cause.

Type/build checks:

- Backend compile check: `cargo check`.
- Frontend type check: `cd ui && npx tsc --noEmit`.
- Frontend production build: `cd ui && npm run build`.

Host verification distinguishes tests from builds. A simple `cargo build`
(optionally one plain `+toolchain`) records Build, not Test, from an owned
native exit; it needs no passing-test count. Help, planning, `cargo check`,
`cargo test --no-run` and arbitrary npm builds are outside this recognition.
A bare runtime success without a native exit is Unknown. In the one-call
`pushd "DIR" && cargo build` form, zero proves both commands ran; a nonzero
exit without owned inner execution is Unknown, even with test-summary text.

The foreground `node scripts/test-launcher.mjs focused -- cargo build ...`
form needs its local request, input and results artifacts to match the
observed owner, root, argv, interval and terminal native stage. Stale,
foreign, interrupted, unrun or inconsistent artifacts cannot supply a pass
or a native-exit reference. Detached focused builds are not supported.
Source, overlap and outstanding-work fences apply to Build as to Test;
this distinction does not change the maintained full gate's Test stages.

## Review Gate

The maintained full gate is one launcher invocation:

```bash
node scripts/test-launcher.mjs full
```

It runs six stages, in fail-fast order: `cargo fmt --check`, `cargo check`,
TypeScript `--noEmit`, the review fingerprint and launcher fixtures, the native
Git Bash Rust wrapper, and the full Vitest suite. Fingerprinting, Cargo and Rust stay rooted at the repository;
TypeScript and Vitest execute with `ui/` as their real working directory so
relative source and fixture paths retain the same semantics as `cd ui`. On
every platform, the effective Cargo is `TERMAL_TEST_CARGO` when explicitly set,
otherwise `cargo` resolved from `PATH`; that same executable is used by the
format check, compile check and passed to `scripts/test-rust.sh`. The format
stage checks Rust using the repository's toolchain and configuration; it never
rewrites source files. Missing rustfmt or unformatted Rust fails that stage
before `cargo check`. Coverage follows Cargo/rustfmt's target and module
discovery; files pulled in only with `include!` are not traversed. On Windows,
TypeScript and Vitest use their JavaScript entrypoints rather than `.cmd` shims. All commands,
working directories and required files are preflighted, and a missing
prerequisite leaves every stage explicitly unrun.

In a registered full run, the exact `cargo fmt --check` stage is a Lint check,
including when it fails. It grants no Test or Build credit. A stage that never
runs stays `not_run` in the run body and mints no verification record. Ordinary
command recognition is unchanged. Lint does not meet a Test-bound criterion.

Before those six stages, the full launcher prepares private UI dependencies
with `npm ci --include=dev --no-audit --no-fund` in the candidate's `ui/`
directory, using its tracked `package.json` and `package-lock.json`. A fresh
worktree needs no manual dependency setup. An existing installation is reused
only when npm's installed lock metadata and package manifests match the source
lock and required entrypoints exist. A stale or partial installation fails setup
without modification; the launcher never reinstalls over an existing directory,
which may be used through inbound links from other worktrees. An outbound
`node_modules` symlink or junction is refused.
The launcher invokes npm's JavaScript entrypoint with its own Node executable,
without installing global tools or building the production UI.

Setup is recorded as the `ui-dependencies` preflight entry and, once npm
starts, `preflight-ui-dependencies.log`. Setup failure produces a terminal
failure with `setupFailure.kind: "ui-dependencies"`, the failing phase, and
every gate stage unrun. A detached full launch announces `STARTED` only after
setup and preflight succeed, so that command waits for a cold install to finish.
Cancellation is recorded as interruption. Recovery refuses while an owned setup
process may still be cleaning up and imports its nonce-matched cleanup ledger
after it exits. Focused commands retain their existing dependency requirements.

### Git prerequisite and remote runners

To run the full gate on a satellite host and report it to the central
coordinator, see [remote full gates](features/remote-full-gates.md).

The launcher's minimum Git capability for locating run artifacts is
`git rev-parse --absolute-git-dir`, already documented in
[Git 2.13.7](https://git-scm.com/docs/git-rev-parse/2.13.7).
It does not require Git 2.31's `--path-format` option or impose a numeric
version gate. This capability requirement does not establish compatibility
with every older Git version for every launcher stage.

Git must return exactly one nonempty absolute path to an existing directory.
The launcher canonicalizes that directory and appends `review-runs` in Node.
For a linked worktree this is its own administrative Git directory, rather
than the common Git directory. Empty, relative, multiline, option-echo or
control-character output is refused with `cannot locate Git run directory`
before any run directory is created or registered.

On a remote runner, check Git from the actual agent process's environment
and repository working directory. An interactive or SSH shell can select a
different executable through `PATH`. These Node commands use the same Git
lookup as the launcher; check both exits and the absolute directory reply:

```bash
node -e "const r=require('node:child_process').spawnSync('git',['--version'],{stdio:'inherit'}); if(r.error) console.error(r.error.message); process.exit(r.status ?? 1)"
node -e "const r=require('node:child_process').spawnSync('git',['rev-parse','--absolute-git-dir'],{stdio:'inherit'}); if(r.error) console.error(r.error.message); process.exit(r.status ?? 1)"
```

### Source metadata, host bindings and raw results

Every run's `results.json` carries a `source` block
(`scripts/test-launcher-source.mjs`) derived from the same admitted owner
observation as `request.sourceId`. It is display metadata, not a second input
identity. `launcherVersion` is read from the measured commit. Until measurement
succeeds the block records `not_yet_measured`.

- `sourceSchema` (1), `commit` and `tree` (`HEAD` and its tree), and `clean`:
  true only when `git status` lists nothing, untracked files included.
- `platform`: `{ os, arch }`, with `os` one of `windows`, `macos` or `linux`.
- `launcherVersion`: the blob of `scripts/test-launcher.mjs` at `HEAD`.
- `measuredAt`, and `toolchain`: `null` until a run that needs Cargo reaches
  its preflight.
- When Git cannot describe the checkout, the identity fields are `null`,
  `clean` is `null` (never true) and `unknownReason` names why, for example
  `git_unavailable`.
- Because system Git config is not read, a Windows checkout that relies on
  Git for Windows' system `core.autocrlf` can show racily-clean files as
  modified, so `clean` is `false` for it. That fails safe: such a run never
  claims a clean start it did not prove.

A run that needs Cargo records the toolchain from its own probes in the run
root: `cargoVersion` and `rustcVersion` (each `--version` output),
`cargoExecutable`, `source` (`PATH` or `TERMAL_TEST_CARGO`), and rustup's
resolution of the active toolchain (`resolvedToolchain`, `rustupReason`:
`default`, `env`, `directory override` or `toolchain file`; `rustupOverride`:
`present` for a directory or file override, else `absent`). Until Cargo's own
preflight succeeds the record says `unknownReason: "preflight_failed"` and
claims no version.

The compiler probed is the one Cargo uses: `RUSTC` or `CARGO_BUILD_RUSTC` when
set (when both are set to different compilers, which one Cargo runs is not
settled here and the version is unknown, `rustc_resolution_ambiguous`);
otherwise the default `rustc`, probed only when it is unambiguous: the
`rustc` beside the chosen Cargo and the one on `PATH` are the same file, or
only one of them exists. When they differ (`rustc_resolution_ambiguous`), the
version is left unknown. It is also left unknown (`rustc_selection_unsupported`),
even when `RUSTC` names the compiler, when the environment sets a compiler
wrapper (`RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER` or their `CARGO_BUILD_`
forms), or when any Cargo configuration Cargo reads (the root's and each
ancestor's `.cargo/config.toml` and `.cargo/config`, and Cargo's home) holds
anything beyond a short toolchain-neutral allowlist: the tables `alias`,
`cargo-new`, `http`, `net`, `profile`, `registries`, `registry`, `source`,
`term` and `target`, and the `[build]` keys `jobs`, `target`, `target-dir`,
`incremental`, `rustflags` and `rustdocflags`. An allowlist, not a list of
known routes, so `build.rustc`, a configured wrapper, `[env]`, `include` and
any setting not yet known all count. The launcher's own tests set
`TERMAL_TEST_CARGO_CONFIG_CEILING` to an absolute fixture directory, where the
ancestor walk stops, so a host's own configuration above the fixture never
decides a test; real runs leave it unset. `RUSTC`, `CARGO_BUILD_RUSTC` and
`RUSTUP_TOOLCHAIN` in the environment are not refused: the probe runs exactly
that compiler, under that same environment. Version lines are read by shape
(`cargo 1.x …`, `rustc 1.x …`, `NAME (REASON)`), so rustup's own messages in
the same log are never taken for them. rustup is asked only when it sits beside the chosen Cargo
(a rustup proxy); a Cargo named directly records `rustup_not_beside_cargo`.
The rustc and rustup probes are provenance only and are recorded under
`toolchainProbes`, not `preflight`, so no failure reader takes them for the
run's failure: a missing or failing one leaves its part unknown
(`rustc_unavailable`, `rustup_failed`, ...) and never fails a gate. A worker
that fails at startup keeps the measured `source`; when it cannot be read
back, the record says `source_unrecoverable`.

`hostBindings` records what ties the stage commands to this host: the absolute
executable each stage's command resolved to (`executables`, by stage name) and
the duration reporter URL. With the request's stages they rebuild every
recorded stage command exactly (`expectedStageEntries`).
`node scripts/test-launcher.mjs plan INPUT_JSON` prints the full gate's stages
and expected entries for the host an input describes (`platform`, `node`,
`gitBash` on Windows, `root`, `runDir`, `reporterUrl`, `executables`), using
that host's path rules, so another platform's run compares exactly. `node` and
`gitBash` are the commands the run's own `request.json` stages name (its
`typescript` and, on Windows, `rust-tests` stages); a reader compares the
plan's `stages` with those request stages and its `entries` with the recorded
stage entries, so an input inconsistent with the run fails one comparison or
the other. It reads and runs nothing; its values are comparison data.

`GET /api/test-runs/{run_id}/results` returns an indexed run's `results.json`
bytes unchanged, with their SHA-256 in `x-termal-results-sha256` and their size
as the content length. A run the index does not know, or one with no
`results.json`, is refused `404` with code `test_run_results_unknown`; a link,
FIFO or other entry that is not a regular file is refused `422`
(`test_run_results_not_a_file`) without being opened; a file above 1 MiB is
refused `413` (`test_run_results_too_large`); a file that changes while it is
read is refused `409` (`test_run_results_changed`); any other I/O failure,
such as a permission error, is `500` (`test_run_results_unreadable`) and is not
worth retrying. A run directory, or its `review-runs` parent, that is no
longer a plain directory (a link or junction put in place after indexing) is
refused `422` (`test_run_results_redirected`), checked before and after the
read; directories above `review-runs` are Git's own administrative ones and are
not inspected. On Windows an ordinary file is opened without following a
reparse point. The response is sent `Content-Encoding: identity`, never
compressed, so the digest and length describe the bytes on the wire. The
route never rescans the index or writes a file (see also
[test runs](features/test-runs.md)).

These link and race refusals are best effort, not containment: the checks are
path inspections around one read, so a writer that swaps the run directory or
an existing Windows reparse file and restores it between them is not stopped.
That is within the route's contract because the route is a diagnostic read,
never evidence by itself: a process that can make such swaps can equally write
`results.json`, these fields are what a run reports about itself, and no record
here grants credit or imports a remote result. A remote import never relies on
these bytes; it uses the producing host's own retained copy.

### Ordinary committed-input runs (`--input commit`)

The launcher measures input through `engram source --root ROOT --json`.
It stores the complete admitted value in `request.sourceId` and `input.json`,
then records actual execution-boundary observations in
`results.sourceIdBefore` and `results.sourceIdAfter`. These closed objects
include root commit/tree, recursive child commits, and root/child local changes.
The shared validator checks shape, bounds and producer ordering; comparison
uses the entire object. No automatic input fingerprint or `.git/info` image is
computed. Explicit review-freeze identity remains separate and unchanged.

In a native TermAl context, the launcher first gets the existing session
`source-owner` metadata pin. The pin supplies executable provenance, not source
input: the launcher still measures its own root. The complete pin stays in the
request's existing immutable registration identity, and executable bytes are
checked around each observation. A refused GET, malformed pin or byte mismatch
stops admission; there is no hostless fallback. A partial `TERMAL_*` context
also cannot become standalone.

For an explicitly hostless run, select an absolute `ENGRAM_SOURCE_BINARY`, or
use the executable under an absolute `CARGO_HOME/bin` (the default is
`~/.cargo/bin`). The launcher never searches PATH or builds Engram. The owner
must implement the complete source_id envelope. Unavailable, malformed or
oversized output is not a partial identity or equality.

By default a clean owner observation selects `input: "commit"` before immutable
registration. A dirty observation selects a plain run and records its reason
in `results.inputSelection`. Equal dirty source values qualify for the input
comparison, including unchanged child changes; plain runs still earn no
committed-input verification. `--input plain` explicitly chooses this lane.
Both lanes require the admitted, before and after complete objects to agree.

`full`, `focused`, `live` and `category` also accept strict `--input commit`:

```bash
node scripts/test-launcher.mjs full --input commit
```

This selection requires clean input and keeps the existing claim, store,
named-root and registration guards. The owner's Git normalization, ignored-path
policy and filter refusals apply. Initialized pinned recursive submodules are
supported. An assume-unchanged or skip-worktree edit that Git status hides is a
documented limitation; boundary equality also cannot detect a transient edit
that is reverted between observations. No raw-byte or case-faithful checkout
claim is made.

A clean committed run records
`input: { kind: "commit", commit, cleanBefore: true }`. After execution,
`input.after` retains `{ commit, clean, paths }` when available, and the outcome
is `clean` or `input_changed`. A newly untracked path is a change, with no new-run
debris/disposition exception. Historical debris records remain readable.
`input.eligible` requires clean committed input, a passing run and equality of
all three complete source values.

A changed or unavailable post-run source cannot produce a passing run or
eligible input. Actual stage states and exit codes stay recorded; an existing
command failure keeps its original failure and exit code. A stage that passed
but changed source leaves a failed, ineligible run. Capture failures before
execution leave stages unrun. Post-run unavailability records a null endpoint
and a bounded reason, never an invented commit or digest.

Summaries show the actual source comparison (`matched`, `mismatch`, or
`unknown`) separately from the stage verdict and gate eligibility. This is
boundary comparison only, not host check credit. A terminal committed run
without eligible input adds `gate: not eligible; not a landing gate: <reason>`.
Exit 0 alone is insufficient for a landing gate.

The launcher's automatic-selection default remains enabled. The host's
independent ordinary-source switch remains false, retaining content-v1 for
ordinary host measurements. The registered committed producer wire remains
`Commit { commit }`; full-source native declared credit and remote completion
are distinct existing contracts. No evaluation, registration or landing policy
is changed by the input comparison.

Once a claim has a registered committed-input run that started on a clean
checkout, whether selected automatically or by `--input commit`, and whatever
its result, evaluations of that claim follow the committed flow for that
claim, fence and naming. A later plain run or `--input plain` does not undo
this. A dirty root is refused until the work is committed and a run passes
on that commit. A claim with no such run stays in the legacy flow.

### Disk space and worktree build-output retention

After registration, before Cargo bootstrap or any stage, the launcher checks
space available to the current user on the filesystems containing the worktree,
run artifacts and Cargo output directories selected by `CARGO_TARGET_DIR` or
`CARGO_BUILD_TARGET_DIR` (otherwise `target/`). A missing output directory is
checked through its nearest existing ancestor; this creates no directories.
Cargo configuration files can select other output locations: their operator
must check those filesystems separately before granting a build window.
Cargo's download/cache directory (`CARGO_HOME`, or its default), the product
test temporary directory (`TERMAL_TEST_USER_TEMP`, or the OS temporary
directory), and other subprocess-specific output locations are also outside
this check. The coordinator checks their capacity separately before granting
a build window, especially when they are on another volume.

The launcher warns below **100 GiB**. A **full gate refuses to start below
50 GiB**, or when available space cannot be established, at any checked
location. Exactly 50 GiB is allowed with a warning; exactly 100 GiB needs no
warning. A refusal saves a failed run with every stage unrun and names the
capacity problem in its summary. Focused runs warn but are not blocked by this
floor. The `live` and `category` modes also warn without this full-gate floor;
`live` can compile Cargo output, so the coordinator must check its build
headroom before granting that run. Foreground and detached workers use the same check. The observation is
not a reservation: other processes can consume space after the check. There
is no supported override flag, automatic cleanup or automatic retry. An
unknown observation includes a safe reason token (never the filesystem error
message); resolve that query failure before running a full gate. A low-space
refusal is a resource condition: preserve it, arrange eligible cleanup in a
build window, then request a fresh run rather than retrying unchanged.

The Merger, or the project coordinator, removes a landed or released
worktree's `target/` in the next agreed build window, after its checks and
evidence have settled. The coordinator also sweeps these eligible worktrees
at each build-window handoff; idle build output must not be retained merely
because the worktree remains. Keep output needed for an open investigation
only with a concrete retention reason and end condition recorded on that work.

Before each removal, verify that no active claim, running check, build or
process uses that output. Resolve the repository root, worktree and target to
absolute paths; require the worktree to be inside this repository's `.tmp/`
and the target to be exactly its `target/` child. Check path-component
boundaries, not just a textual prefix. Refuse symlinks, junctions or other
reparse points in the path or anywhere beneath the deletion target. Inspect
the final resolved path before recursive deletion, and remove only that
explicit target with non-interactive filesystem operations. Never remove
the main checkout's `target/`, a running executable, active worktree output
or run evidence. Bulk deletion waits while either project's gate runs.
Leave any ineligible output untouched and report the exception to the
coordinator for a separate plan within the repository-folder safety rules.
Record the removed paths and recovered space in the tracker. The launcher
only reports capacity; it never deletes build output.

For an authorized focused check, pass an argument array after `--`:

```bash
node scripts/test-launcher.mjs focused -- node --test scripts/test-launcher.test.mjs
```

For named UI test files on Windows, run Vitest's JavaScript entrypoint from
the repository root, including `--root ui` and the files' Vitest project:

```powershell
node scripts/test-launcher.mjs focused --input commit -- node ui/node_modules/vitest/vitest.mjs run --root ui --project component src/transcript-activity-slot.test.tsx src/panels/SessionActivityStrip.test.tsx
```

This form needs installed UI dependencies and a running host with direct-Node
Vitest classification. It records the registered stage as Test from its actual
argv, through the existing committed-input verification and Engram receipt
checks. A stage label cannot supply that classification. It does not add Vitest
counts to the launcher's ordinary focused Node/TAP count reader. `--root ui`
selects Vite's root; tests that themselves depend on the process working
directory can use the category route for execution in `ui/`; category runs
remain partial checks and do not earn registered Test credit.

The supported direct CLI path ends in `node_modules/vitest/vitest.mjs` and is
followed by `run`, file filters and optional `--root`, `--config`/`-c`,
`--project`, `--testNamePattern`/`-t`, `--reporter`, `--outputFile`,
`--maxWorkers` or `--testTimeout` values (separate or `=value`). Other modes,
watch/help flags and unknown options remain unclassified in a focused run.
Windows `.cmd` package-manager shims are not needed.
As with `npx vitest`, this classifies the trusted installed runner's invocation;
it does not attest the package bytes or confine the runner's configuration.
File-writing reporters should target ignored scratch (such as `.tmp/`), since
creating an untracked report makes committed input ineligible.

A direct `focused -- cargo test` skips `scripts/test-rust.sh`, so unless something
sets a count libtest runs one test thread per available core, where `full` defaults
to `RUST_TEST_THREADS=4`. On a many-core host a direct run is far harsher than a
gate: on a 144-core host one selection failed two tests at the default and passed
all of them at 4. The focused stage of `results.json` records the setting as
`testThreads`, and the summary prints it as `test threads:`. It reports only what
the launcher can see. A direct `cargo test`, or `scripts/test-rust.sh` run directly,
reports its count or that libtest chooses. Another command that runs libtest, or a
setting whose effect isn't certain, such as Cargo configuration naming
`RUST_TEST_THREADS`, reports `unknown`, so read the count from the test process. A
command that shows no sign of running libtest reports nothing. State it with any
timing or failure you report from a run. Don't set it to
get a failing test to pass: a test that only passes at 4 threads is a finding. The
default stays as it is because failures like those only come to light there.

To run one UI test category on its own, name it after its Vitest project (see
[UI test categories](#ui-test-categories)):

```bash
node scripts/test-launcher.mjs category ui-heavy
```

The categories are `ui-unit`, `ui-component`, `ui-heavy` and `ui-app`, read
from `CATEGORY_PROJECTS` in `ui/test-categories.ts`; an unknown or missing name
is refused before any run exists. A category run is a partial check, not the
full gate: its `request.json` and `results.json` carry a `scope` saying so,
`full` stays `false`, and the summary names that scope before its stages. A
category run uses the same Vitest configuration as the full gate, with
`--project` added.
Passing category runs never add up to a full gate. TermAl does not credit a
category run as a test check.

The full gate's Vitest stage, and a category run, record one child row per UI
project under the stage's `accounting`, taken from that same run's JSON report
(`scripts/test-categories-plan.mjs`): the files the project selects and the
ones that ran, its test counts, and whether its files' test windows overlapped
and the projects ran in their declared order. The rows are children of the one
stage, not further stages. A project that failed, ran nothing, left a selected
file unrun or a test unfinished, or ran files whose test windows overlap, a
file that ran outside the plan, a file result that is not whole (no file
status, test list, test statuses or valid times), and a report that is missing
or unreadable each fail the stage, even when Vitest exited 0; rows that cannot
be established are `unknown`, never passed. A file whose tests were all skipped
has no test window and is left out of the order and overlap checks. `ui-heavy`
is the serialized lane: the Vitest configuration runs its files one at a time
in a group of their own. The accounting is a backstop to that: it sees only
the reported test windows, not collection or environment setup.

Name a shell explicitly when one is required. On Windows, direct `.cmd` and
`.bat` executables are rejected; use Node with the package's JavaScript CLI or
an explicit known native shell. No launcher preset installs dependencies,
builds the production UI, touches `ui/dist`, restarts a host, or accesses live
store policy.

Each run owns a unique directory below Git's `review-runs` metadata directory.
Foreground and detached launches print `RUN RUN_DIRECTORY` after the initial
request write, before registration or any stage executes, so an
interrupted host wait can still inspect or settle the run without rerunning it.
In a TermAl session, the launcher requires a versioned durable registration ACK
and an exact host query before admitted source capture, preflight, Cargo bootstrap
or stages, then prints `TRACKED RUN_ID`. A detached worker rechecks its receipt,
immutable request and host visibility before preflight. Registration failure
ends the launch with a nonzero result and an explicit error. Ambiguous lost POST
replies permit a bounded retry with the same identity; definitive refusals,
malformed replies and unconfirmed queries never admit execution.
Registration diagnostics distinguish POST/GET headers, body reads and identity
validation, retain known HTTP status through error-body read or cancellation
failure, and include a bounded host reason when available. The same operation
nonce and attempt ordinal correlate launcher and host diagnostics; the run id
and request digest identify the immutable input. Host diagnostics include phase elapsed
time, StateInner acquisition/hold time, and available fence progress/error.
The host's claim/durability allowance is 60 s. The client funds that window,
an admitted held-read overrun and delivery: 123 s ordinary, 247 s with the source pin.
Host phase traces are transient (16 completed phases and 8 lock observations,
with error JSON detail capped at 2048 bytes); launcher errors remain in the run
results. These diagnostics do not prove COMMIT, settlement or a safe restart,
and host stderr alone does not provide retained history after a lost response.
Only an invocation with none of `TERMAL_BASE_URL`, `TERMAL_SESSION_ID` and
`TERMAL_CLI` may run standalone and print `UNTRACKED RUN_ID` with a reason.
A partial context cannot downgrade to standalone. The `RUN` receipt identifies
the initial request, whose registration and admitted source_id are added later;
neither `RUN` nor `TRACKED` is a passing test result.

The host also exposes read-only `GET /api/sessions/{id}/source-owner` for a
visible native local root session. It derives the project itself and accepts
no query or body parameters. Its closed response contains `binaryPath`,
`sha256`, `binaryVersion` and `sourceVersion`: the canonical configured native
Engram executable, its measured digest and actual version, and the source JSON
schema validated from an `engram source --root <project-root> --json` response.
It returns no source verdict or store details. A well-formed unavailable source
response can establish schema support; it cannot establish clean-source evidence.
One admitted phase plan funds version/preparation and source measurement,
each with at least 60 s plus 2 s process delivery, including pin checks and parsing.
Existing longer version deadlines stay longer. The default aggregate is 124 s;
the metadata GET's client allowance adds five seconds for HTTP delivery (129 s).
The host resolves a configured native command in its own
absolute PATH directories, rejects distinct competing programs, and returns an
absolute pin. It never searches cwd, relative PATH entries or batch shims;
the launcher performs no PATH discovery. Missing or relative path configuration, changed bytes, malformed
output, unsupported schema, stderr, process failure and expiry refuse the probe.
No user/global configuration change is involved.

An optional `sourceOwner` object with those same four fields in `request.json`
joins the immutable request identity. The host independently verifies it against
the initiating session's project configuration before registration, rechecks
configuration and executable bytes before publication, and persists the pin
without changing historical registrations. This source probe has the shared
124 s metadata plan; the outer host deadline adds a 60 s claim/durability
allowance. Claim/durability receive that allowance after probing, within the
outer deadline. The registration client also funds a full admitted held-read
overrun and response delivery: 123 s for ordinary registration and 247 s with
the source pin. Those claim windows use `scripts/engram-call-budget-ms.json`,
shared with the host's minimum operation window rather than its source budget.
The server's publication deadline is unchanged. Omitting the field preserves legacy identity and behavior;
the outer registration POST remains `runId` plus `runDirectory`.
Pinned registration requires a visible native local root session, including on
replay. An ACK retry also requires the configured executable still to match its
saved pin; upgrading or replacing it refuses the retry instead of relabelling
the historical registration. The GET endpoint has its own two-permit
limiter, independent of review-freeze verification capacity. On Unix, script
wrappers are refused before probing: the configured file needs a native ELF
or Mach-O header as well as successful bounded version/schema probes.
This additive bootstrap does not yet make the launcher call the endpoint or
switch the committed-input predicate. The later switch requires the endpoint
to be present in the running host. Installing a build does not change the
running host; the established restart procedure still applies.

The source command's additive v1 response includes a required `source_id`.
On a complete measurement it is a closed object with `commit`, `tree`,
`submodules` and `changes`. Each submodule has its root-relative `path`,
actual `commit` and repo-relative `changes`; expected pins are implied by
the committed parent tree. Each change requires `path`, `status`, `origin`,
`blob` and `mode`. Origin is null except for a rename or copy. Blob and mode
are null for an absent path; a staged deletion whose path still exists
contains its current blob and mode. Modes are strings `100644`, `100755`
or `120000`. Ignored files and gitlink placeholders are excluded from the
file changes. Arrays retain the owner's UTF-8 byte order; two porcelain
records for one path, such as `??` followed by `D `, remain separate.
An unavailable measurement has `source_id: null`, never a partial value.

The ignored Rust test
`source_owner_live_shared_fixtures_preserve_complete_producer_identity` runs the
real pinned owner through the host adapter against the producer's shared
`sourceCases` and `createSourceFixture` library. Set
`TERMAL_TEST_LIVE_ENGRAM_BINARY` and `TERMAL_TEST_LIVE_ENGRAM_SHA256` to the
candidate's recorded absolute path and digest, and `TERMAL_TEST_SOURCE_FIXTURES`
to an absolute module exported from that same owner candidate's
`scripts/source-input-fixtures.mjs`. Keep that module and fixture scratch inside
the TermAl repository. The test honors the owner's platform skips, creates
repository-local disposable roots, compares the entire runtime-derived
`fixture.sourceId`, and prints actual identities with their case names.
The first measurement consumes the same validated source observation that
establishes the pin's source schema, rather than issuing a second full read.
It retains that invocation's root and observation time; a separate GET or later
capture cannot relabel it as fresh evidence. Subsequent measurements reuse only
the executable pin and perform a fresh source capture. Each observation has its
own sixty-second deadline, including its byte checks and parsing. An expired
caller cannot admit new work; an admitted observation gets at least sixty seconds.
Fixture construction uses the existing finite Git setup budget. Run this test
only in an authorized focused slot; it is not part of the default FULL suite.
Each measurement prints bounded operation timings and its remaining budget.
The separately ignored
`source_owner_live_child_fixture_reports_observation_costs` test uses the same
module and binary/digest variables to create exactly one fresh `child-dirty`
fixture through the owner's factory. It measures the first and subsequent
observations once each against that fixture's complete expected identity. This
is a new measurement, not recovery of a missing failed fixture or its timings.
There is no failed-capture retry, producer timeout change or performance
threshold. Both tests retain their actual fixture parent on panic through the
existing test-root retention mechanism and print its path for investigation;
successful fixtures are removed normally.

The host's `engram_source_id_until` takes an explicit executable pin, an
already-authorized root and the caller's absolute deadline. It returns
the complete typed value, including local changes, after byte-pin checks,
bounded execution and strict parsing. A single admitted plan retains the
absolute phase deadlines through queues and nested helpers; it is not floored
again at each nesting level. Version/preparation and source measurement each
keep at least 60 s plus process delivery, and existing longer deadlines remain.
The source command omits `--timeout-ms`, letting either owner use its supported default.
Before the Engram sync, the source owner's own default is 30 s; TermAl no longer
imposes a source limit and waits at least 60 s plus overhead.
This does not claim a 60 s end-to-end owner limit before the sync.
Source deadlines derive from `scripts/source-id-budget-ms.json` and the shared
operation/delivery pair in `scripts/engram-call-budget-ms.json`.
Calls still pending after ten seconds log an operation-named warning without
aborting or resending. Launcher source reads are asynchronous so this diagnostic
can run while the owner process is working.
Outer process delivery adds 2 s and metadata HTTP adds 5 s. Pinned registration
funds source measurement and the separate 60 s host claim/durability stage,
with the admitted held-read overrun and client delivery allowance described above. Native opening
likewise funds the 124 s metadata plan and reserves 60 s for authority work.
Reclamation has a 60 s read pass within a 130 s disposition allowance counted
from the pass's start, including the read and its admitted overrun;
its scheduling cadence and retry hints remain independent of these limits.
JSON is limited to one MiB, with one trailing LF allowed outside that limit.
Unavailable, incoherent, oversized or late input is an error with no fallback.
Equality compares every field of this object, excluding the envelope's
root path, status and provenance metadata. This addition does not switch
the launcher or credit consumers, rewrite stored records, add a watcher,
or change the launcher's current clean-commit policy. The host and owner
changes are integrated together before this extended response is used.

For an approved independent clone, create or select a local project for that
clone **on the runner's host**, and start the native runner session in that
project. Registration checks the session's configured project Git common
directory against both the run directory and the launcher's request root.
Changing only the command's working directory to a second clone leaves the
session bound to the original project and is refused before a stage starts.
Sharing a Git remote URL does not authorize another clone. Keep the native
session's inherited `TERMAL_*` context; do not substitute a Central session's
identity or weaken the run-directory guard.

A mismatched project binding surfaces as HTTP 400, `test run directory is
outside this project's run directories`, in `validate_target_directory`.
For example, a session bound to an outer repository cannot register a run in
an independent clone nested under that repository: the clone has a separate
Git common directory. Select the clone's own local project and native session.
Other invalid or redirected paths can produce the same refusal; the message
alone does not identify which binding or path was wrong.

This is origin-host registration, not remote verification credit. The origin
host's index and on-host wait can see the run. A coordinator can also register
a wait using `host:<origin host UUID>::<native run id>`; a bare id remains local
and never searches other hosts. Both hosts must support native completion
observations over their existing authenticated relay, and both projects need
the same logical project key already validated by their Engram connection.
The wait needs neither a local run directory nor a preissued job or mailbox
request. Missing registration, project association or native terminal facts
remain pending; a remote path or raw `results.json` cannot supply a success.
The origin retains the completion only through its existing registration and
positively correlated native execution settlement. These remote observations
are explicitly uncredited; remote verification admission is a separate contract.

Keep the existing completion mailbox notice through `--notify` to a different
coordinator as the fallback for older hosts or unavailable completion delivery
(see [detached gate delivery](#detached-gate-delivery)).
Keep the runner's original inherited `TERMAL_SESSION_ID`; do not create a
registration on another host or relaunch the run to make it appear in that index.

`request.json` records the exact plan and complete admitted `sourceId`, plus
`detached` and the creating process's `creatorPid`, which TermAl's test-run
index reads (see [test runs](features/test-runs.md)); `results.json` records
actual process exits, explicit unrun stages, timestamps,
and full log paths. Terminal JSON replacement is atomic for concurrent readers
on the same filesystem; it is not a claim of power-loss or crash durability.
On Windows a replacement cannot happen while a reader holds the file open, so
it waits for the reader within a 2 s budget, retrying only the rename of the
complete temporary file, and fails with the original error once the budget is
spent.
Diagnostic extraction is bounded and does not decide success. A missing
terminal result is `UNKNOWN`, never a pass. Source drift before execution
stops the command. Post-run drift or unavailability prevents a passing run;
actual command failures keep their outcome and exit code. `execution.lock`
prevents rerunning the same plan.

Old artifacts retain their recorded fingerprints as attributed history. They
are never accepted as proof for a new source_id run. Explicit review-freeze
commands, reviewer identities, test-ref names and Review-Freeze trailers keep
their existing fingerprint format; this cutover does not change them.

<a id="detached-gate-delivery"></a>

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

For a focused stage, recovery also refuses while its recorded process owner
may be alive and cleaning up. Once the owner has exited, recovery imports its
nonce-matched cleanup ledger when available; a missing ledger remains
`unresolved`. Recovery never converts an interrupted test into a pass or
reruns it. Older runs and non-focused stages can still have a child that
outlived their launcher; their raw logs need not be final after recovery.
If the recorded pid has been reused by an unrelated process, `recover` keeps
refusing and the run stays `UNKNOWN`, which is never a pass: start a new run
instead of editing its evidence. If a killed `recover` left `recovery.lock`
behind, remove that file by hand once no other `recover` is running.

When a command is launched through TermAl's native Windows terminal or bounded
reader with confirmed job membership, the host owns a kill-on-close job and
ends its members after observing root exit. An `unavailable` launch can run
without that job. This host containment is separate from the focused
launcher's command owner described below. `recover` itself never kills a
process or creates ownership retroactively.
See [Windows command containment](features/windows-command-containment.md) for
the reported status, packaged-process limits and retained-handle fixture proof.

### Focused command ownership and stall notices

Every focused stage has a separate process owner. On Windows, a detached Node
guardian starts built-in Windows PowerShell without detaching that shell.
Windows PowerShell cannot initialize reliably under DETACHED_PROCESS; the
guardian preserves independent lifetime while its ordinary child can run.
PowerShell loads the checked-in C# owner and creates the command atomically in
a non-breakaway, kill-on-close job, with an explicit inherited-handle list.
It resumes the command only after attachment. Failure to establish that job
refuses execution; there is no uncontained fallback. PowerShell compiler
scratch uses a per-owner unique directory under the helper checkout's
`.tmp/ow/`, independently of linked-worktree run-directory depth. Its absolute
path is limited to 200 characters to leave room for generated compiler names;
a longer helper checkout is refused with a diagnostic. Existing `.tmp` and
`ow` components must be real directories canonically inside the helper checkout;
redirecting junctions or symlinks are refused before allocation. The guardian
rechecks those components before cleanup, then removes
the scratch after PowerShell exits and records removal or retention in the
bootstrap breadcrumbs. If the guardian itself is killed, scratch may remain
for worktree cleanup. The command retains the caller's environment.
No package or toolchain is installed. This requires Windows
PowerShell and permission to load its C# helper; restricted environments that
forbid that helper receive a failed run rather than partial containment.
The guardian records its PowerShell child PID as well as its own. If the
launcher dies, the guardian forwards private-pipe EOF and drains closed output
until native cleanup publishes its ledger. If the guardian itself dies,
Node's private job closes its PowerShell child, closing the strict job handle.
Bootstrap breadcrumbs alongside the owner request identify shell entry,
compilation, native entry and ledger publication; a silent shell exit cannot
be mistaken for command execution.

On macOS and Linux the separate Node owner uses a dedicated command process
group. A descendant that deliberately leaves that group is outside this POSIX
boundary; Windows descendants cannot request job breakaway. Neither platform
enumerates processes by executable name. The unrelated processes of another
run are never termination targets.
This owns descendants created in the command's process tree, not work started
through an external service such as WMI, a scheduler or the service manager.
The POSIX boundary also depends on its guardian staying alive: killing that
guardian itself (for example by SIGKILL or the OOM killer) can leave its group
alive. Windows job-close containment also covers guardian death. A POSIX host
without a working orphan reaper may retain zombies; a still-visible group is
conservatively reported unresolved, never assumed reaped.

Ctrl-C/SIGINT or SIGTERM requests focused shutdown. A forced launcher exit
(including a runner deadline that terminates it) closes the owner's private
control channel, so its owner also shuts down the tree. Teardown is idempotent
and observes termination for at most five seconds. Windows confirms
zero active job members; POSIX checks its process group. The final immutable
`focused.log.owner-result.json` records the command PID, trigger, ownership
kind and `reaped` or `unresolved` cleanup. Its PID list is a best-effort job
snapshot on Windows and the command root only on POSIX, not a census of all
descendants. Job inventory failure cannot skip termination. A failure or
missing proof is never a pass. This adds no deadline to a test: the cleanup
budget starts only after interruption or command exit. A normal command exit
also ends any descendants it left behind.
The bounded summary exposes cleanup, owner exit diagnostics and the latest
stall observation. Bootstrap failures retain the owner's native exit code,
signal and bounded stderr even when no final cleanup ledger was written.
Cancellation observed before spawning the owner starts no command; an interrupt
during native bootstrap can race command creation, but still requires owned
cleanup and can never become a passing result.
The CLI installs these signal handlers only during the owned command phase.
Before or after that phase, signals retain their ordinary termination behavior;
recovery can then settle any missing terminal result without rerunning it.
On Windows, buffered bootstrap cancellation is checked directly on the private
pipe before creation and again before resuming a created command; it does not
depend on scheduling a background reader task.

While a focused command runs, stdout and stderr still go directly to its log.
The launcher checks the log's byte count once per second. Five minutes without
new bytes produces a `STALLED` notice naming the run, stage, command PID,
inactivity duration and log path, and saves the observation on that stage of
`results.json`. This is output inactivity, not a diagnosis of the command.
It applies to a quiet build as well as a quiet test. New output records
`resumed` and starts a new inactivity interval; one notice is emitted per
quiet episode. Tests control the clock rather than waiting five minutes.

Use `--notify COORDINATOR_SESSION_ID` on a focused invocation to send each
stall notice to that coordinator too. Without a notification target, the
durable observation and foreground holder notice remain available. A notice
uses a separate idempotency key and a bounded notification attempt; it never
consumes the eventual completion key. A stalled command stays running until
it exits or its owner explicitly interrupts it. No stall notice kills, retries
or reruns a stage, changes a test timeout, supplies test counts, or declares a
terminal result. Inspect the original run with `summary`; interrupt its owning
execution if needed, then use `recover` only after the launcher and command
owner have exited.
The 15-second notification budget includes its process-owner bootstrap; a
failed attempt is recorded and reported without an automatic retry. Failure to
save a progress observation is reported to the holder without crashing the
launcher; that quiet episode remains eligible for a later observation.
Resume delivery is retried on later polls even without further output, so a
failed resume save cannot suppress the next quiet episode.

The lifecycle fixtures stay under this checkout's `.tmp/launcher-lifecycle`
to keep agent-created files inside the repository even when an outer runner
uses a system temporary root. Passing fixtures are removed by the repository
helper; failed fixtures are retained for the holder's process audit and causal
investigation, outside the managed run-root sweep. On Windows both launcher
SIGTERM and SIGKILL fixtures use native process termination; they do not prove
delivery of a POSIX signal. Unix additionally registers a real SIGINT case and
asserts that both catchable signals pass through the launcher's abort handler.
Those checks require a Unix run for native evidence. Injected POSIX state-machine checks cover IPC and
cleanup-budget failures and do not establish native Unix process behavior.

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

The whole suite includes the cross-project launcher row in the
[ordinary evaluation matrix](engineering/ordinary-evaluation-matrix.md).
Before running the whole suite or selecting that row, set these process
environment variables from the reviewed Engram source manifest:

- `TERMAL_TEST_ENGRAM_LAUNCHER_ROOT`: an absolute local Engram source repository.
- `TERMAL_TEST_ENGRAM_LAUNCHER_COMMIT`: its exact full 40-hex source commit.

The row clones that source into its disposable fixture and uses Engram's own
maintained launcher. It never edits the source repository. These inputs are
separate from the executable pin; the row checks them and records them in its
`MATRIX_ENGRAM_LAUNCHER_SOURCE` output. Missing inputs fail the row. Other named
rows do not require them. They are not additional launcher CLI flags or entries
in `results.json`.

Live stages always pass `--nocapture` to libtest so passing-test provenance and
diagnostics survive in `engram-live.log`, including the matrix's actual check,
evaluation, completion and source-attribution records. The launcher still
returns a bounded summary while retaining the complete stage log.

To run only some of the live tests under the same pin, name them after `--`:

```bash
node scripts/test-launcher.mjs live --engram-binary C:/absolute/engram.exe --engram-sha256 SHA256 -- tests::engram_host_adapter::root_recovery_live::named_root_completion_live::recovered_claim_check_in_a_newly_named_root_completes_without_an_edit
```

Each name is the test's full libtest path, as `cargo test --bin termal --
--ignored --list` prints it. The live suite's tests sit in nested modules, so
no module prefix is assumed. A name must lie inside the `root_recovery_live`
module. A malformed or repeated name is refused before any run directory
exists. The named tests run with `--ignored --exact --test-threads=1 --nocapture`,
after the same verification as the whole suite. The stage passes only when libtest
reports a result for every named test and its counts cover exactly those tests.
A name that matches nothing fails the run, which names the tests that did not
run, so a filter never passes having run no tests. The request, results and
summary (`live tests: …`) record the names. With no names the stage runs the
whole suite exactly as before. The host recognizes a live run only in the
foreground: a detached `live` run is not credited.

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

The wrapper's `TERMAL_TEST_FD_LIMIT` is a **test-harness** policy, not the
application's `TERMAL_NOFILE_LIMIT` startup policy. The latter defaults to 8192,
caps overrides at 1,048,576, and is inherited by newly spawned agent runtimes.
See [runtime descriptor capacity](architecture.md#process-descriptor-capacity)
and the [shared Codex app-server](features/shared-codex-app-server.md#descriptor-capacity).
The `process_limit_tests` Rust filter exercises injected limit reads/writes,
environment selection and diagnostics on every platform, including Windows;
it never changes the test process's real resource limit or environment.
That proves policy, not a native macOS syscall or live app-server capacity.

The wrapper requires Node.js (the version in `.nvmrc`) on `PATH`, including
`node.exe` when WSL launches Windows Cargo. Node owns the temporary run directory,
redirects `TMP`/`TEMP`/`TMPDIR` into the product's `termal/tests/run-*` folder,
reports newly escaped artifacts, and removes successful runs without retries.
A failed run is retained for diagnosis; removal failures name the path, OS error
and surviving entries. A green run whose run root still holds entries fails too,
and its root is retained: every test must remove what it created. A root made by
`TestTempRoot::create` has a sibling `<root>.owner` file naming the test that
created it, so the report names the test behind each such leftover; other
leftovers are reported by name only. Stale marked runs are swept only after the
recorded processes have exited and the age threshold has passed.

Direct `cargo test` and `cargo check` do not require Node. Direct tests use the
Rust product-temp helper where adopted, but bypass the wrapper's environment
containment, end-of-run audit and marked-run sweep; use the wrapper for the full
gate while raw temp call sites are still being converted. Both Node and Rust
helpers require absolute paths without `..` components. A manually supplied
`TERMAL_TEST_RUN_ROOT` must be a direct `run-*` child of the product tests folder.
Paths are normalized lexically, not canonicalized through filesystem aliases;
product and run directory components must not be symlinks or junctions.

The review-integrity helper tests run on Linux, macOS, and Windows in CI: the
same maintained suites as the full gate's `fingerprint-tests` stage
(`helperTestFiles` in `scripts/test-launcher.mjs`), which a launcher test keeps
in step with the workflow.

The launcher's own tests (`scripts/test-launcher.test.mjs`) wait for the
processes they start by events: an exit or close, a readiness message, or a
barrier file. No deadline decides whether one passed, so a slow host makes them
slower, not failed. A hang in any helper test is surfaced by one liveness
guard, the runner's per-test timeout (`--test-timeout`, set by
`HELPER_TEST_HANG_GUARD_MS` in `scripts/test-launcher.mjs` for both the stage
and CI). It is sized as a hang detector, not a performance bound: 600 s, over
13 times the slowest helper test measured on a loaded gate host (about 45 s,
2026-10-09). A timeout does not stop what the hung test started, so
`--test-force-exit` ends each test process once its tests have finished; the
fixture processes waiting on a barrier see their owner gone and exit too, and
the stage ends with the failure instead of waiting on them. A product process
stuck outside a barrier is not stopped and outlives the stage. Force-exit also
means a passing test that leaks a handle no longer keeps its suite running, so
leak detection needs its own check. CI runs these suites on the gate host's
Node (`.nvmrc`), whose runner these rules were checked against.

A launcher test the guard stops is reported as `hung: <test>`, and a failed one
as `failed: <test>`. Either keeps its fixture directory, with its worker logs,
and names its path on stderr, so a slow worker and a hung one can be told apart
(`scripts/test-fixture-cleanup.mjs`); a passing test removes it. The other
helper suites still remove their fixtures unconditionally. The
`fingerprint-tests` stage runs `node` directly, without the Rust wrapper's run
folder, so a kept directory lies in the product test folder itself
(`termal/tests` under the user temp folder), gate or not; with
`TERMAL_TEST_RUN_ROOT` set it lies in that `run-*` folder instead. It survives
the current run, but not indefinitely: every launch of the wrapper (including
the `rust-tests` stage of any later full gate) sweeps the product test folder,
removing a direct entry once it is 48 hours old, and a marked `run-*` folder
once it is that old and its recorded processes have exited, at most 64 entries
per launch. Copy the logs elsewhere, for example to `.evidence/`, if they must
outlive that. A Node older than `.nvmrc` may lack the test context's `error`,
and then reports a hang as `failed`.

The Vitest resource preflight itself uses three fixed CPU samples and the median,
so one scheduler spike does not reject a gate while sustained starvation still
fails before frontend tests start. Windows reports process CPU availability
without presenting its unsupported load-average value as real system load.

### Rust panic journal

libtest keeps a test's panic output in memory and prints it when the test
finishes. When a second panic during unwinding aborts the test process (on
Windows, exit code `0xc0000409`), that output is lost with the process, and so
is the name of the test that started it. So the launcher gives every command in
a run two variables:

- `RUST_BACKTRACE=1`, unless the caller already set `RUST_BACKTRACE` (in any
  case on Windows). With it, the launcher also sets `RUST_LIB_BACKTRACE=0`,
  unless the caller set that one. That is std's "panic backtraces only"
  pairing: without it, every `anyhow` error would also capture a stack, under
  std's process-wide backtrace lock.
- `TERMAL_TEST_PANIC_JOURNAL`, the run directory's `rust-panics.jsonl`, also
  recorded as `panicJournal` in `results.json`.

When that variable is set, the test binary installs a panic hook once, as it
loads (`src/test_panic_journal.rs`). For each panic the hook appends two JSON
lines, joined by process id and sequence number:
1. The panic line: time, message, location and a backtrace.
2. The thread line: the thread's name. libtest names a test's thread after the
   test.

Reading the thread's name is the one step that can still fail fatally, so the
panic line is on disk before it. The hook writes straight to the file, outside
libtest's capture, and then hands the panic to the hook before it, so libtest's
own report is unchanged. Without the variable, nothing is installed or written.

A Rust stage ends abnormally when its test process did not exit as a pass or an
ordinary libtest failure.
- Through cargo: cargo itself exits 101 either way, so the launcher reads
  cargo's `process didn't exit successfully: …` line for a test binary (under
  `deps/`) whose status is not 101, or a signal. A build script's failure does
  not count.
- A command that runs the test binary directly: it crashed (a signal, or a
  Windows status code such as `0xc0000409`) and the run's journal exists.

For such a stage, the run summary prints the journal's path and its last three
panics, each with its thread and the first lines of its backtrace. The full
records stay in the file. Panics that tests provoke on purpose are recorded
too, but they are listed only after an abnormal end.

The journal's own regression (`src/tests/test_panic_journal.rs`) crashes a child
copy of the test binary on purpose, in the test's temporary directory, so a core
file never lands in the repository. On Windows and macOS the operating system
may still record a crash report for it outside the repository.

Known gap: when `scripts/test-rust.sh` runs `cargo.exe` through WSL interop, it
forwards only `RUST_TEST_THREADS`. So there is no journal and no backtrace
setting, and after an abort the summary says no panic was recorded.

### Per-test durations

The full gate names every test that took 2 seconds or longer, so slow tests
are found and repaired instead of failing under load. The 2 s figure is a
repair-report budget. It never decides whether a stage passed, and no timeout
is raised to meet it. Two stages write a machine-readable duration artifact
into the run directory beside their human log:

- `vitest` adds Vitest's JSON reporter beside its default one and writes
  `vitest-durations.json`;
- `fingerprint-tests` keeps Node's `spec` output in its log and adds
  `scripts/node-test-duration-reporter.mjs`, which writes the runner's test
  events to `fingerprint-tests-durations.jsonl`.

After each of those stages, `results.json` records a `durations` report:

- how many tests were measured, and how many had no duration;
- the tests at or above the budget, with file, full name and state (failed
  and timed out are kept distinct from passed);
- the files whose test time reached 10 seconds.

Both lists keep at most 200 entries each, the slowest first, with exact totals
beside them. Names and paths are kept to one line of at most 300 characters,
so `results.json` stays within the size the host reads. The artifact keeps
every test.

The durations are the runners' own figures, and the summary says so. A
test's time includes its own per-test hooks (`beforeEach` and `afterEach`),
but not import or environment time. A file's time is the runner's file figure
and is never divided among tests: for Vitest, it runs from its first test's
start to its last test's end, with the hooks between them included. A skipped
or todo test is not counted. The summary lists at most ten such tests and five
such files per stage, each artifact path on its own line, and the artifact
holds the rest.

`rust-tests` reports its per-test durations as unavailable, because stable
libtest prints none; only the stage time is recorded. A missing or malformed
artifact is reported as `missing` or `unreadable`, never as a pass or a
failure. The report logic is in `scripts/test-durations.mjs`.

### Credit for a background or detached gate

A full gate takes longer than a Claude tool call may run (10 minutes), so a
session that needs the gate as evidence for an Engram item it holds launches
it in the background, in the item's named source root, with committed input:

```bash
pushd "C:\path\to\worktree" && node scripts/test-launcher.mjs full --input commit
```

with the Bash tool's background mode, then ends the turn. A runtime that can
notify a coordinator may launch it with `--detach` instead.

Credit comes only from registered verification. A run registered with committed
input, automatically or with explicit `--input commit` ([committed-input runs](#ordinary-committed-input-runs---input-commit))
in the claim's named source root, on the exact store, that ends with terminal
results and an eligible committed-input record (`input: commit <sha> clean;
eligible`), is sent to Engram with `verification_record` over the session's
work session, and Engram decides credit from that record. A run without that
registration, or whose record is not eligible, earns no credit, whatever its
stages say. Only a root session holding the claim can name its root; a
delegated session is measured in its workdir and has no named root.

A terminal verification refusal is stored beside the run in
`engram-verification-record-state.json`. The launcher `summary` shows that
refusal separately from the native test verdict, including Engram's returned
field and reason when supplied. A passing test can therefore still have a
rejected verification record. Both the run command and each check command
have Engram's 8192 UTF-8 byte limit; a longer command is reported as
`verification_command_too_long`, with its field and byte count. Commands and
their fingerprints are preserved, and a terminal refusal is not resent.

Retained Commit-format verification bodies are retired locally before any
route read or send, with `legacy_source_input_retired`. Their original bytes remain readable;
the local disposition does not determine any earlier producer outcome. Existing
receipts remain readable without converting the body to a current identity or
creating new legacy producer observations.

A committed-input registration in a named root reads that exact claim's
current held association before it publishes the registration. A same-claim
handoff can retain the original name: the current holder's held read must prove
the same work, claim and recovery run against the canonical bound naming proof.
The original namer, naming time and generation remain unchanged, as do earlier
registrations. The original namer's identity is not authority for the launching
session. A complete reply proving the caller holds no matching claim leaves
its launch unbound and ineligible for claim-bound credit, as before; uncertain
or inconsistent authority still refuses registration.
An unavailable Engram service or an unavailable selected claim refuses the
launch before tests start; a
changed claim fence requires naming the root again. This read shares the
registration's sixty-second claim/durability budget. Replay keeps the original
association and does not read the claim again. Automatic selection does not
downgrade or retry a refused committed registration. Its error preserves the
cause and suggests renewing the claim or naming the root again, or explicitly
choosing `--input plain` for a run without verification credit. A runner with no
matching named root stays unbound; it does not read a held claim for that absent
binding and earns no claim-bound verification credit.

TermAl still carries such a launch past the turn as local custody, and says
before the next prompt when a launch is not carried or a carried one is
dropped: a launch joined to another command, a `--detach` launch that failed,
a launch less than two minutes after an unmatched one in the same worktree, a
launch with no named root or in another worktree, a write TermAl can observe
reaching the worktree while the run is going, a released claim or a renamed
or cleared root, or six hours without an ended run. That custody is not
verification: it adds no credit and settles nothing at a later turn. While the
run is going, a command the holding session runs in that worktree counts as a
write unless the session is a Claude session and the command only reads (such
as `git status`, `git diff`, or `node scripts/test-launcher.mjs summary
RUN_DIRECTORY` from the repository root). A host restart during the run loses
the carry, and TermAl says so after it. An acceptance evaluation requested
before the run's record is sent is judged without it, and Engram marks it
stale when the record lands, so request it after the record is sent. The full
rules are in
[the Engram host adapter's carried background gates](features/engram-host-adapter.md#carried-background-gates).

The watcher omits `Modify(Any)` and metadata-only notifications whose path
currently identifies a plain directory, with readable, non-link ancestors.
Windows can emit these for directory attributes or security metadata without
changing a child file. This is not a `.claude` exclusion: child-file events,
creation, deletion, rename, unknown events and unreadable or linked paths
remain observable. A file write followed by a restore still fences the run;
equal final hashes or old mtimes do not cancel an observed file event.

One gate is no longer recorded for the other items of a commit that lands
several items: TermAl sends no `verification_bind` for them, so each item
needs its own registered run. See [batch full gates](features/batch-full-gates.md).

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
a block, never let an older pass stand. A focused run is outside this rule
and has its own, below; the stage name `focused` is reserved for it, so a
full gate's stage of that name is never a test stage.

A launcher run that never reached a stage tested nothing, so its failure is
no test's, whatever its mode. A launcher that refuses before its run starts,
as when its registration is aborted, prints only `FAIL launcher: …` and exits
1. With neither the run's verdict `PASS|FAIL RUN exit=CODE` nor a stage line
`NAME: passed|failed` in its output, its check ends with outcome Unknown, in
the one-call form and the plain form alike. A run refused before its first
stage after its summary was printed, such as by the disk-capacity floor above,
prints `FAIL RUN exit=1`, but its own `results.json` records it failed with
every stage unrun. In the foreground, when the summary's first verdict names
the run whose `results.json` its `results:` line names, its check ends
Unknown. A carried run like that is dropped, never recorded. A
run that failed at any stage, its first included, is still recorded failed.

Registered verification classifies focused stages from their actual runner
argv. The stage label `focused` alone no longer selects Test: an unsupported
command produces an explicit unclassified-stage refusal. Declared native
Flutter/.NET credit is reserved to the original host-observed native route;
a generic focused launcher registration cannot adopt a declaration or a
later artifact as that execution's evidence.

The exact focused argv `node scripts/check-doc-links.mjs`, with no arguments,
is a registered Lint check. Run it from the named root with
`node scripts/test-launcher.mjs focused --input commit -- node scripts/check-doc-links.mjs`.
Both passing and failing outcomes are recorded as Lint; it creates no Test
observation or Test/Build credit. No declaration or test artifact is needed.
The launcher's resolved Node executable path (including `node.exe` on Windows)
is accepted. Script-path variants, shell wrappers, renamed scripts and extra
arguments are not this route. This requires a host build that implements this classification.

The existing committed-input evaluation admission accepts an acknowledged,
passing Lint-only record on the current clean named commit. It still requires
the original binding, source, body/digest and host-observed receipt; a failed
or unacknowledged record cannot admit the evaluation. This does not satisfy
a criterion bound to Test verification, and requesting evaluation does not
create an acknowledgement.

A successful arbitrary command, such as `node scripts/check-doc-links-renamed.mjs`,
can therefore leave a registered run at `pending: unclassified_stages`.
TermAl builds no verification record for that run: waiting or repeating an
evaluation request cannot make Engram acknowledge it. Evaluation refuses
with `verification_record_unclassified_stages` and names the unsupported
classification instead of asking the holder to wait for receipt. This
decision reads the run's request and terminal results directly, including
before the sender first examines it or if its progress file is missing;
requesting evaluation does not prepare or send a record.

Other documentation checkers have no classification merely because they read
Markdown. Do not label them as tests or builds. Apart from the exact Lint
route above, supported routes are actual recognized test/build commands, or a real
test using [declared native verification](features/declared-test-commands.md#native-verification).
The latter needs a committed `termal-tests.toml` entry, the exact declared
command as the first tool action of a fresh turn, and a supported TRX or
Dart test artifact, along with the native source and authority checks.
Adding a declaration does not convert an old generic launcher run into
that execution or supply its missing record. Supporting an honest additional
check class requires a producer/consumer verification contract; it is not
enabled by changing a stage label.

A focused run, `node scripts/test-launcher.mjs focused -- COMMAND ARG...`,
is a test check only when its wrapped command is itself one (a `cargo test`
or a bounded native `node --test`, not a build or a `--no-run`). Its exit status alone says nothing about how
many tests ran, so the launcher reads the wrapped runner's own result lines,
cargo's `test result:` lines summed across test binaries or one complete,
coherent native Node TAP/spec totals block, records them
on the focused stage of `results.json` as `tests` (`passed`, `failed`,
`ignored`), and prints them in its summary as `tests: passed=N failed=M
ignored=K` beside the source_id boundary comparison. A runner it does not recognise gets
no counts. A focused run earns credit only as registered verification with
committed input, selected automatically or with `--input commit` as above;
otherwise the host reports it as
an uncredited check, and reports it passed only from that run's own
`results.json`, found through the summary's `results:` line under the same
checks as a full gate's request record: the summary's own verdict, its first
`PASS RUN exit=0` line, names that same run; the run and its focused stage
passed with exit 0; and the counts show at least one test passed and none
failed. Counts printed in the summary but missing from the record earn
nothing, and a `results:` line printed later in a stage's diagnostics cannot
lend another run's counts. A failing focused run is recorded as failed, as
before. A focused run started with `--detach` is no test check at all: only a
full gate is carried. A passing focused run
is reported like the plain test command it wraps, never as a full gate. The
host side takes effect once a host built with it is running, that is after
the next TermAl restart. Native Node recognition and unsupported-form feedback
also require that updated running host; declarations alone cannot add them.

Native Node focused checks additionally match owner, repository root, exact
inner argv, execution interval, native terminal exit and equal complete
admitted/before/after source_id values through the same local artifact validator
as focused Build.
Missing, partial, duplicate or inconsistent totals are not reported as passed;
skipped/TODO tests are ignored rather than executed passes, and cancellation
counts as failure. Valid matching failing evidence reports a failed test;
missing or mismatched evidence remains unknown, never a summary-text rescue.
The bounded Node grammar supports TAP/spec reporters, name/skip patterns,
positive concurrency/timeout values and test files; watch, help, eval and
arbitrary inner scripts are not native Test forms. An unsupported foreground
focused command receives a command-identifying notice that no Test record will
follow. Ordinary unrelated Node commands receive no launcher-specific notice;
recognized Build commands retain their separate Build lifecycle.

### Quoted test filters

A quoted pipe in a test filter qualifies only on an unwrapped command whose
runtime dialect is known. Currently that is Claude's Bash tool, whose reader
supplies Bash provenance; single, double and mixed quotes use that Bash
argument reading, including the test in the supported one-call form. Codex
and ACP observations do not establish a dialect. A `bash -c`, `pwsh -Command`
or `cmd /c` wrapper containing a pipe remains ineligible, even when its text
looks quoted: an outer shell or wrapper may transform those quotes. Unknown
runtime lines also remain ineligible. A `.cmd` or `.bat` program
(case-insensitive, including a
path-qualified runner) never qualifies: `cmd.exe` can parse the literal pipe
again. Bare or executable `npm`, `pnpm`, `yarn`, and `npx` also stay ineligible
because their downstream script shell or shim is not established by Bash
provenance. The same restriction applies to the wrapped runner after a
test launcher's `focused --`. A native `cargo`/`cargo.exe` or the supported
Node test invocation keeps its quoted-filter behavior. No PATH resolution,
package-manager version, or script-shell configuration is inferred here.
The parsed argv is retained through recognition, placement and verification;
focused Node evidence still needs
its matching owner, argv, interval and input artifacts and cannot fall back to
generic stage/count text. Full gates with a qualified pipe argument are not
carried, and a detached completion must match its started invocation.
This bounded form refuses any `$`, `&`, `;`, `<`, `>`, backtick, CR or LF
anywhere in the command, including inside quotes. It also refuses unquoted
`*`, `?`, `[`, `]`, `{`, `}`, `(`, `)`, `^`, and word-leading `~` or `@`,
escaped unquoted pipes, incomplete escapes and unterminated quotes. Thus
filters such as `'a$|b$'` and `'a;b|c'` do not qualify. Real pipelines and
unsupported ANSI-C/locale quotes remain refused. Use supported
repeated filter flags or separate independent commands where this bounded
form does not apply. This requires a host containing the recognition change.
See the [host adapter's test evidence contract](features/engram-host-adapter.md#test-evidence).

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

### UI test categories

`ui/test-categories.ts` places every UI test file in exactly one of four Vitest
projects, which run in this order, one file at a time:

- `unit`: a `*.test.ts` file that needs no DOM (explicit list);
- `component`: every other `*.test.tsx`, plus the `*.test.ts` files listed as
  needing the DOM;
- `heavy`: a component file with a test at or above 2 s in measured runs, with
  real-time sensitivity, or that exercises the virtualizer and measurement
  lifecycle (explicit list, each with a reason and its resource tags:
  `cpu-heavy` for measured duration, `wall-clock` for real-time waits);
- `app`: a file that renders the whole App (explicit list).

The `unit` project runs in a node environment, with no DOM. A unit file whose
run reaches a DOM global node lacks, such as `document` or `window`, itself or
through a module it imports, fails and belongs in the list of files that need
the DOM. Two cases are not caught that way. Browser storage is one, because
`ui/src/test-setup.ts` supplies in-memory `localStorage` and `sessionStorage`
in every project. Code behind a `typeof window` or similar check is the other,
because it takes its non-DOM branch. The other three projects run in jsdom. Each project has its own `sequence.groupOrder`, starting at 1.
Vitest 4 runs a one-worker, isolated project with groupOrder 0 after every
ordered group, so 0 would put it last.

The guard `ui/src/test-categories.test.ts` checks the placement rules:

- A new `*.test.ts` must be added to the unit list, to the list of files that
  need the DOM, or to `app`.
- A file that renders the App must be in `app`, and every `app` file must
  render it. A file renders the App when it imports the App module, calls a
  harness export that renders it (`renderApp`, `renderAppWithProjectAndSession`
  or `withFallbackStateHarness`), or imports a local module that does. This is
  a text match followed one module level deep, not an import graph.

It also fails when any of these hold:

- a test file under `ui/` falls into no project or into two, by the manifest's
  own matcher;
- a selection pattern is a glob other than the one component glob;
- the group order is wrong;
- a project's environment is not node for `unit` and jsdom for the others, by
  the manifest, by a source check of `ui/vite.config.ts`, and by the guard's
  own run, which is in `unit` and asserts that `document` and `window` are
  undefined;
- a unit file's own text contains one of these patterns: an import
  `from "@testing-library/…"`, `renderHook`, `render(`, `screen.`,
  `document.`, `window.`, `localStorage`, `sessionStorage`, or a
  `@vitest-environment` comment, which would give that one file jsdom inside
  the `unit` project.

That last check is a static backstop with stated limits. It reads only the
test file's own text, not the modules the test imports, and only those
patterns. A unit file that reaches a DOM global through a module it exercises
passes it; the `unit` project's node environment catches such a file instead,
by failing its run, with the two exceptions above (browser storage and code
behind a typeof check).

## Known Coverage Gaps

The active follow-up list lives in Engram. Use the `engram` `next` tool for
currently unblocked work and `ls` for the wider open inventory.
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
