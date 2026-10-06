# Declared Test Commands

**Status: PLANNED, NOT YET AVAILABLE.** This document describes a contract whose host side is not implemented. Declared-command recognition requires a running TermAl build that implements it; this documentation landing alone does not enable it, and installing a build does not change the host that is already running. Until the running host implements declared-command recognition, the declaration adds no recognition; existing built-in commands retain their current behavior. TRX and JUnit availability are stated separately below.

A repository tells TermAl which commands are its tests by committing one file, `termal-tests.toml`, at the repository root. When an agent runs a declared command exactly as declared, TermAl records it as a test check and judges it from the result artifact the command writes, never from console text. The runner is not TermAl's: the agent runs its real command through its own runtime, and TermAl observes.

Related: [Test Runs](test-runs.md) (the launcher's runs and cards), [Engram host adapter](engram-host-adapter.md) (how checks become verification evidence).

## The declaration

`termal-tests.toml`, UTF-8, at the root of the worktree the agent's shell is in. TermAl reads the current worktree file when a declared command starts, keeps the matched entry and the file's SHA-256 on the check, and verifies both again when the command ends. If the declaration file or the matched entry differs at completion, the result is UNKNOWN. An uncommitted edit is honoured: the declaration is trusted current-worktree configuration, like a build script, reviewed as code under the repository's own rules; TermAl does not verify that a declared command is a real test.

| Field | Meaning |
|---|---|
| `command` | The exact command line the agent will run, compared token for token with the content of quoted arguments kept. On the declared route a quoted `;`, `\|` or `&` is literal argument text (the TRX logger needs `trx;LogFileName=…`); unquoted shell operators, command substitution, redirection, several commands and ambiguous quoting are refused. TermAl's built-in recognition and its simple-command rule are unchanged. |
| `cwd` | Directory the command runs in, relative to the repository root. Default `"."`. Must resolve inside the worktree. |
| `artifact` | The one result file the command writes, relative to the repository root. Format is read from content: TRX (`TestRun` root) or JUnit XML (`testsuites`/`testsuite` root). |

A file that does not parse, exceeds the size or entry caps, an entry missing `command` or `artifact`, a `cwd` or `artifact` resolving outside the worktree, a `command` with an unquoted shell operator, substitution, redirection or ambiguous quoting, or one containing `--no-build`, `--list-tests` or a `.dll` path, disables the whole file; TermAl says so once in a host line.

## Exact match

A command is a declared test check when: the agent's shell is in the entry's `cwd` (on Claude, the one-call form `pushd "DIR" && <command>`, nothing piped, redirected or chained after it; on a runtime that reports its directory, that directory); the normalised line equals exactly one entry's `command`; and the command is not one TermAl recognises on its own (for example cargo, `npm`/`pnpm`/`yarn test`, `npx vitest`, `npx jest`, pytest, `go test`, the test launcher): built-in recognition wins and is unchanged. Zero or two matches is no check. A different flag, filter or logger name is a different command.

## The artifact

When the command starts, TermAl looks at the declared path and records what is there: absent, or the bounded hash and modification time of the file. When the command ends:

- a file artifact must be a regular file at the same canonical path, different by hash from the start image if one was recorded, and modified at or after the start;
- the artifact path must be untracked and git-ignored at its canonical in-root target, judged in the same Git context TermAl uses for the source basis (a tracked file counts as source even when an ignore pattern matches; a link must not lead from an ignored path to a tracked file);
- the file is at most 4 MiB and is read once as one image; it is hashed from that image.

With exit status 0, anything else is UNKNOWN with an observed reason: start inspection failed, unchanged since the start, missing at the end, not ignored or tracked, outside the worktree, too large, changed during read, not a regular file. The modification-time rule is an additional heuristic, never proof; this is correlation, not proof that the command produced the file. An unchanged file means the completed artifact has the same bytes as the start image; the host cannot distinguish reuse from a byte-identical rerun (a TRX file carries a per-run id and timestamps, so this is not a realistic outcome of a real rerun).

Why git-ignored: TermAl measures a turn's source by the content of every tracked and untracked-not-ignored path. A result file that is not ignored would change the source revision of the very run that wrote it, and the check would be voided as "source changed". Add the result directory to `.gitignore` (for example `.termal-results/`).

## Outcomes

TRX: TermAl parses one complete, well-formed `TestRun` document and its direct `ResultSummary` and `Counters`, with no DTD or external-entity resolution.
- **PASS**: the command's exit status is 0; `ResultSummary` has a recognised completed non-failure outcome with no run-level error or abort; `executed >= 1`, `passed >= 1`; `failed`, `error`, `timeout`, `aborted`, `passedButRunAborted` are all 0; the counters are consistent as real TRX files are (derived from captured files, including a mixed pass/skip run); no counter with unknown meaning is non-zero.
- **FAIL**: the run outcome is Failed, Error or Aborted, or any supported run-abort indication is set, or any of `failed`, `error`, `timeout`, `aborted` is ≥ 1 — even when the exit status is 0; or the exit status is not 0, whatever the file says.
- **UNKNOWN**: everything else — zero executed, all inconclusive or not executed, truncated or malformed XML, a missing `ResultSummary` or `Counters`, an unknown outcome word, an unknown non-zero counter, any artifact rule failed, declaration changed.

A run can report passing tests and still have failed as a run (for example an attachment error): the run outcome decides, not the counts alone. A non-zero exit is FAIL even when the artifact is missing or unreadable. Precedence: a declaration that changed during the run is UNKNOWN; otherwise a non-zero exit is FAIL; otherwise the artifact rules and the counters decide.

JUnit (availability: lands with TRX only if the whole remaining two-day budget fits after the TRX part is green; otherwise as the immediate follow-up item): complete XML and one unambiguous aggregate (`testsuites` attributes, or the `testsuite` elements summed once). **PASS**: exit 0, `tests - skipped >= 1`, `failures = errors = 0`, levels consistent. **FAIL**: `failures + errors >= 1`, or exit not 0. **UNKNOWN**: zero or all skipped, contradiction, unknown dialect. The reader is proven on the one producer captured; other producers' files may read as UNKNOWN until a capture of theirs is added.

A passing result is ELIGIBLE for verification; the host's source, ownership and overlap checks still apply, and the recorded verification is authoritative. A declaration by itself grants no credit. PASS is never inferred from exit 0 alone or from console output. UNKNOWN is not a weaker PASS: it earns nothing and names what was observed.

## Examples

.NET under VSTest (`dotnet test`), one project and one target framework per entry (a fixed report name across several target frameworks overwrites; the Testing Platform writes TRX through its reporting extension with `--report-trx`, a different command):

```toml
[[test]]
command  = "dotnet test tests/Orders.Tests --logger \"trx;LogFileName=results.trx\" --results-directory tests/Orders.Tests/.termal-results"
cwd      = "."
artifact = "tests/Orders.Tests/.termal-results/results.trx"
```

One project and one target framework per entry: a fixed `LogFileName` across several target frameworks overwrites the report. This exact line is recognised only by a running host that implements declared commands.

JUnit from a project-provided wrapper (direct `npx vitest` and `pytest` are built-in checks and never take the declared route): the wrapper runs the real tests synchronously, propagates their exit status, and writes exactly that one complete JUnit file:

```toml
[[test]]
command  = "node scripts/project-tests.mjs"
cwd      = "."
artifact = ".termal-results/junit.xml"
```

`.gitignore` in both cases: `.termal-results/`.

## Workflow

Operator, once per repository: write `termal-tests.toml`, ignore the result directory, commit both, run one declared command once and check that the check appears with its counters; an UNKNOWN reason here points at the declaration, the result path or the ignore rule.

Agent, each run: read `termal-tests.toml` at the root; run one entry exactly as declared, from its `cwd`, in the recognised one-call form; then read the host's line or the check record. A passing result is eligible for verification; the recorded verification is what counts. UNKNOWN gives an observed reason, not a diagnosed cause: on "missing", check that the logger flag and result path match the declaration; on "not ignored", check the ignore rule; on "zero executed", check what the command selects. Do not retype the command with other flags to make it match, and do not edit the declaration to fit a command already run: fix the declaration, commit it under the repository's rules, run again.

Snippet for a repository's own agent instructions:

"Test commands are declared in termal-tests.toml at the root. Run one exactly as declared, from its cwd, as a single call (`pushd "DIR" && <command>`). TermAl judges it from the result file it writes; a passing result is eligible for verification, UNKNOWN is not PASS and names what was observed. This requires a running TermAl build that implements declared commands."

## What the pilot does not do

- It does not capture the environment of the agent's shell; TermAl cannot see it, and the artifact route does not need it.
- It does not parse console output on the declared route; a declared run without an artifact is never PASS: it is UNKNOWN, or FAIL on a non-zero exit. Built-in runners are unchanged.
- It has no framework or version matrix: the reader accepts the documented TRX result shape; a declaration is not a framework/version certification.
- It trusts the declaration as current-worktree configuration; it cannot tell a real test command from a script that writes a plausible file.
- It observes absence or the existing bounded file hash at command start and compares the completed artifact: correlation, not proof that the command produced it.
- It reads the declaration at command start and keeps only the matched entry and the file hash on the check; no declaration history or lifecycle.
- JUnit follows the fit rule above; the Status line says what the running build implements.
- Nothing here changes which commands TermAl recognised before, nor any rule for commit, push, tracker or approval.
