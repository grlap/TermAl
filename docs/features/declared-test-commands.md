# Declared Test Commands

**Status: IMPLEMENTED ON MASTER SINCE `2b1e3f85` (TRX).** Declared-command recognition requires a running TermAl host built from that commit or later. Installing a build does not change the host that is already running; recognition becomes available after restarting onto a build that contains the implementation. TRX passed post-restart technical qualification on 2026-10-09, host build `f5a96582`: passing and failing xUnit runs, an unmatched command, and a deleted artifact were observed. This was a disposable fixture qualification, separate from the team pilot's onboarding checklist. Dart/Flutter JSON support additionally requires a build containing its reader; its live proof follows installation and restart. A host without the implementation gains no recognition from a declaration; existing built-in commands retain their current behavior. TRX and Dart test JSON reporter streams are implemented; JUnit XML is not yet implemented.

A repository tells TermAl which commands are its tests by committing one file, `termal-tests.toml`, at the repository root. When an agent runs a declared command exactly as declared, TermAl records it as a test check and judges it from the result artifact the command writes, never from console text. The runner is not TermAl's: the agent runs its real command through its own runtime, and TermAl observes.

Related: [Test Runs](test-runs.md) (the launcher's runs and cards), [Engram host adapter](engram-host-adapter.md) (how checks are reported, and where test credit comes from), [Supervised pilot onboarding](supervised-pilot-onboarding.md) (how a colleague's repository declares its dotnet tests for the pilot).

## The declaration

`termal-tests.toml`, UTF-8, at the root of the worktree the agent's shell is in. TermAl reads the current worktree file when a declared command starts, keeps the matched entry and the file's SHA-256 on the check, and verifies both again when the command ends. If the declaration file or the matched entry differs at completion, the result is UNKNOWN. An uncommitted edit is honoured: the declaration is trusted current-worktree configuration, like a build script, reviewed as code under the repository's own rules; TermAl does not verify that a declared command is a real test.

| Field | Meaning |
|---|---|
| `command` | The exact command line the agent will run, compared token for token with the content of quoted arguments kept. The declared route reads a line by an allow-list, assuming bash or PowerShell 7.3 or later (`pwsh`) runs it: the line is printable ASCII, with words separated by spaces and tabs; the program, the first word, is unquoted and holds no `=`; an unquoted word holds only letters, digits and `. _ / : = @ + -`, starts with neither `@` nor `=`, and holds no `.` if it starts with `-` (PowerShell splits such an argument; quote it); a quoted word is one whole, non-empty token whose quote opens and closes at a separator, holding the same characters plus space, `;` and `=`, so a quoted `;` is literal argument text (the TRX logger needs `trx;LogFileName=…`). Anything else (shell operators, substitution, redirection, globbing, commas, `%`, `!`, `"` inside quotes, a backslash anywhere, mixed quoting, non-ASCII) is refused. Use forward slashes in declared paths (`./tests/Orders.Tests`, not `.\tests\Orders.Tests`): bash reads a backslash as an escape. In this build a `!=` test filter is therefore not declarable: use a positive filter (`TestCategory=Fast`, unquoted — a single-quoted argument is not recognised when Codex runs the entry, because the script of its `pwsh` wrapper may hold no `'`) or a runsettings file. The declared route refuses `cmd` and Windows PowerShell 5.1 (`powershell`) as the first program of the bare line or an accepted wrapper script; it also refuses a recognized Cmd dialect. TermAl does not follow commands invoked indirectly by another program. The line is read from its raw bytes: it must be printable ASCII from end to end, and only ASCII spaces and tabs are trimmed from its ends; CR, LF and every other control character except tab are refused before trimming. A wrapper counts only in an exact shape, every word of it as written: its shell named by a bare name or an absolute path that the program-word rule allows (unquoted, no `=`; not a `.cmd`, `.bat` or `.ps1` shim), and its option words spelled exactly, without quotes: `bash`, `sh` or `zsh` with one flag, `-c` or `-lc`, then the script; or `pwsh` with any of `-NoProfile`, `-NonInteractive`, `-NoLogo`, then `-Command` or `-c`, then the script; the script is one single-quoted word. Codex's Windows executable envelope also permits a whole double-quoted drive-absolute Windows path ending in `pwsh.exe`, such as `"C:\Program Files\PowerShell\7\pwsh.exe" -Command '<line>'`; the path uses letters, digits and `. _ / @ + -`, plus spaces and backslashes, with a colon only after the drive letter. The same pwsh option and single-quoted script rules apply. This exception applies only to the outer executable path: backslashes, quoting and all other characters in the inner declared line still follow the unchanged line rules above. Other quoted executables, relative paths and mixed quoting are refused. Command matching is lexical: TermAl does not resolve the program through PATH or run it. On Windows the Flutter SDK exposes `flutter` and `dart` as `.bat` shims; these real SDK invocations still use the same literal argument allow-list, without shell operators or expansion. The outer shell-wrapper restrictions above remain unchanged, as do built-in recognition and its simple-command rule. |
| `cwd` | Directory the command runs in, relative to the repository root. Default `"."`. Must resolve inside the worktree. |
| `artifact` | The one result file the command writes, relative to the repository root. The file content selects TRX (a `TestRun` XML root) or the Dart test JSON reporter (newline-delimited JSON objects), regardless of filename. JUnit XML (`testsuites`/`testsuite` root) is not read yet, see below. |
| `env` | Optional list of required environment **names**, for example `["SUPABASE_TEST_URL"]`. Names follow the local project environment rules below; an invalid or duplicate name, or a reserved `TERMAL_`/`ENGRAM_` prefix, disables the whole declaration. Values belong to the executing host's local project settings, never this declaration. An omitted or empty list preserves the existing behavior. |

### Local project environment

An operator can replace a local project's environment settings through
`PATCH /api/projects/{id}/environment`, with an `environment` object mapping
names to values. The endpoint stores them in that host's existing TermAl state
store outside the repository and returns the ordinary state snapshot without
the values. It accepts at most 64 names, 8 KiB per value and 64 KiB in total.
This PATCH replaces the entire map; omitted names are removed. The response
does not list configured names. Keep the intended name set when replacing it.
Names use ASCII letters, digits and underscores and begin with a letter or
underscore; TermAl and Engram identity names are reserved. No OS user or global
environment is changed. Configure a remote project's values on its executing
host's local project; Central's proxy cannot supply or forward its own values.
Settings are trusted launch configuration: names other than the reserved host
identities can override provider launch variables, PATH and loader variables.

Claude and ACP receive the selected values in their session process. Shared
Codex uses its existing thread-scoped shell policy at thread start/resume,
not the shared app-server process environment. Updates take effect at the next
runtime start/resume; they do not change an active runtime or a retained run.
Values are excluded from client/federation snapshots, diagnostic text and
source identity or declared-run request hashes. The local persistence image
retains the settings; it must be protected like the rest of the host's state.

Before handing a prompt to the provider, TermAl gives a bounded names-only
notice for missing or blank required values.
Claude-native slash commands retain their original text; the notice is
deferred until the next ordinary prompt so commands such as `/compact` work.
It does not prevent an agent's
shell command from executing. Instead, a matched entry with a missing value
settles as **UNKNOWN**, with `environment_missing:<NAME>`, regardless of its
exit or artifact verdict, and receives no native credit. This check uses the
environment TermAl selected for that runtime, never values inherited from the
host or set independently in an agent shell. The missing-name decision is
retained from the original command start. Command matching, artifact checks,
source comparison and completion wakes otherwise retain their existing rules.

A file that does not parse, exceeds the size or entry caps, an entry missing `command` or `artifact`, a `cwd` or `artifact` resolving outside the worktree, or a `command` the route cannot read (above) disables the whole file; TermAl says so once in a host line. Runner options such as `--no-build`, `--list-tests` and assembly paths do not disable a declaration: the validated result artifact must establish that tests actually ran.

## Exact match

A command is a declared test check when: the agent's shell is in the entry's `cwd` (on Claude, the one-call form `pushd "DIR" && <command>`, nothing piped, redirected or chained after it; on a runtime that reports its directory, that directory); the line's words, read as above with their quotes taken away, equal those of exactly one entry's `command`; and the command is not one TermAl recognises on its own (for example cargo, `npm`/`pnpm`/`yarn test`, `npx vitest`, `npx jest`, pytest, `go test`, the test launcher): built-in recognition wins and is unchanged. Zero or two matches is no check. A different flag, filter or logger name is a different command.

## The artifact

When the command starts, TermAl looks at the declared path and records what is there: absent, or the bounded hash of the file, and the canonical path it leads to. When the command ends:

- a file artifact must be a regular file at the same canonical path, different by hash from the start image if one was recorded, and modified at or after the start, with 2 seconds of slack: file-system timestamps come from a coarser clock than the host's (FAT keeps two seconds), so a file written just after the start can read as just before it;
- the artifact path must be untracked and git-ignored at its canonical in-root target, judged in the same Git context TermAl uses for the source basis (a tracked file counts as source even when an ignore pattern matches; a link must not lead from an ignored path to a tracked file);
- the file is at most 64 MiB and is read once as one image; it is hashed from that image. A larger file gets a named bounded-reader refusal; this limit is separate from the source identity JSON bound.

With exit status 0, anything else is UNKNOWN with an observed reason: start inspection failed, unchanged since the start, missing at the end, not ignored or tracked, outside the worktree, too large, changed during read, not a regular file. The modification-time rule is an additional heuristic, never proof; this is correlation, not proof that the command produced the file. An unchanged file means the completed artifact has the same bytes as the start image; the host cannot distinguish reuse from a byte-identical rerun (a TRX file carries a per-run id and timestamps, so this is not a realistic outcome of a real rerun).

Why git-ignored: TermAl measures a turn's source by the content of every tracked and untracked-not-ignored path. A result file that is not ignored would change the source revision of the very run that wrote it, and the check would be voided as "source changed". Add the result directory to `.gitignore` (for example `.termal-results/`).

## Native verification

For registered native verification, the declared command must be the turn's first tool action. Its frozen result artifact supplies the verdict, including for commands that also have built-in console recognition. Full source identity before the prompt and after the run must match; identical local changes are allowed. Existing execution identity, claim, root, store and artifact checks still apply. Workspace watcher events do not decide native verification.

Acceptance admission keeps the host's clean committed-source policy. A native
run can admit evaluation only when its authenticated terminal passed, Engram
acknowledged the exact frozen verification body, and the complete SourceId
matches the owner's current clean source. The complete identity stays pinned
through evaluator capture and submission; a matching commit alone is insufficient.
A native record with local changes can still be recorded, but it refuses
acceptance admission: commit the intended changes and run the declared test again
on that commit. It does not fall back to an older passing result or legacy
admission. This policy does not change Engram's SourceId requirements or create
test/build evidence from other checks.

Provider output persistence or truncation, a background launch or move, a missing exit code, and a task-completion notice without a verdict do not override a valid artifact. The host does not read provider console spill files for native verification. A background launch waits for its original task's completion before freezing the artifact and capturing after-run source, even when the artifact is already complete; a process killed before its artifact END is frozen cannot earn PASS. The original task keeps its reservation and any identity refusal when a later prompt begins.

The following exit-based rules describe the legacy observed-check route. Native verification uses the artifact's PASS, FAIL or UNKNOWN directly and preserves the exit only as an observation.

For a host-observed declared Claude task, the original task-completion notice
also freezes and reads the artifact using the declaration captured at its
start, even when native verification was not admitted. This covers both an
explicit background launch and a foreground call the runtime moved to a
background task. Launch acknowledgement alone does not finish the check. The
task notice supplies no usable exit or success report: a complete PASS artifact
therefore leaves the ordinary check UNKNOWN with a named missing-exit reason.
A FAIL artifact yields FAIL; a missing or invalid artifact yields UNKNOWN with
its artifact reason. The exit field stays empty. Duplicate notices keep the
first terminal result. An interrupted task cannot supply PASS. Native
verification continues to use the frozen artifact verdict directly.

This ordinary check outcome is separate from native credit. A newly untracked
declaration can be recognized for an ordinary check while native verification
reports `declaration_not_committed`; a dirty edit of an already committed
declaration path is a different case. No reservation or verification record is
created by settling the ordinary task. A restored older observation that lacks
its original declaration binding remains UNKNOWN rather than reading a new
declaration at completion. Foreground completion retains the observed-check
rules below. Claude's explicit successful tool result (`is_error=false`, with
no background move) is an observed success even without a numeric exit, and
continues to use the existing exit-aware judge with the artifact. Persisted
output does not remove that success report. A foreground error result without
an exit number remains UNKNOWN with a named missing-exit reason; the host
does not open the provider's console spill file to reconstruct an exit.

Foreground declared observations use these same legacy rules in projects
without Engram, or with execution observability disabled. Their original start
binding supplies the artifact; a completion never reconstructs a missing
binding from the current declaration. The run row and completion wait remain
pending while the artifact is judged, then expose the applied verdict. An
older row without its original binding ends UNKNOWN with a named reason.

## Legacy observed-check outcomes

TRX: TermAl reads one complete `TestRun` document and its direct `ResultSummary` and `Counters` with the quick-xml reader, with no DTD or external-entity resolution, and checks: one root and closed elements with matching end tags; XML characters only, and XML Names for elements, attributes and processing-instruction targets (not `xml`); unique, quoted, whitespace-separated attributes without `<`; only the predefined entities and valid character references; no `--` in comments and no `]]>` in text; an XML declaration, if any, first and with its version (UTF-8 only); only XML whitespace, comments and processing instructions outside the root; and the counter consistency below. It is not a complete XML 1.0 validator, and it is not a security boundary: the declared command is trusted, and it can always write a well-formed passing file.
- **PASS**: the command's exit status is 0; `ResultSummary` has a recognised completed non-failure outcome with no run-level error or abort; `executed >= 1`, `passed >= 1`; `failed`, `error`, `timeout`, `aborted`, `passedButRunAborted` are all 0; the counters are consistent as real TRX files are (derived from captured files, including a mixed pass/skip run); no counter with unknown meaning is non-zero.
- **FAIL**: the run outcome is Failed, Error or Aborted, or any supported run-abort indication is set, or any of `failed`, `error`, `timeout`, `aborted` is ≥ 1 — even when the exit status is 0; or the exit status is not 0, whatever the file says.
- **UNKNOWN**: everything else — zero executed, all inconclusive or not executed, truncated or malformed XML, a missing `ResultSummary` or `Counters`, an unknown outcome word, an unknown non-zero counter, any artifact rule failed, declaration changed.

A run can report passing tests and still have failed as a run (for example an attachment error): the run outcome decides, not the counts alone. A non-zero exit is FAIL even when the artifact is missing or unreadable. Precedence: a declaration that changed during the run is UNKNOWN; otherwise a non-zero exit is FAIL; otherwise the artifact rules and the counters decide.

JUnit (not implemented in `2b1e3f85`; proposed follow-up contract): complete XML and one unambiguous aggregate (`testsuites` attributes, or the `testsuite` elements summed once). **PASS**: exit 0, `tests - skipped >= 1`, `failures = errors = 0`, levels consistent. **FAIL**: `failures + errors >= 1`, or exit not 0. **UNKNOWN**: zero or all skipped, contradiction, unknown dialect. A JUnit reader must be proven on captured producer output; other producers' files may read as UNKNOWN until a capture of theirs is added.

### Dart and Flutter JSON file reporter

Use the real `dart test` or `flutter test` command with
`--file-reporter json:<path>`. `--reporter expanded` may be used alongside
the file reporter; console output never decides the result. The reader uses
the [Dart test JSON reporter protocol](https://github.com/dart-lang/test/blob/master/pkgs/test/doc/json_reporter.md),
version 0.1.1, proven by captures from Dart 3.13.2 and Flutter 3.47.2.
Other protocol versions, event types or test result words are UNKNOWN.
A new SDK may need a reader update; an unsupported version fails closed
until its output is supported. Extra fields on known events are allowed.

- **PASS:** exit 0, a final `done` event with `success: true`, at least one
  non-hidden, non-skipped `testDone` with result `success`, and no failure
  or error anywhere in the complete stream.
- **FAIL:** a nonzero exit; otherwise, in a complete supported stream, any
  `testDone` result `failure` or `error`, any `error` event (including a
  hidden load/setup test or an error after a successful `testDone`), or
  `done.success: false`.
- **UNKNOWN:** missing or null-success `done`, truncated/non-UTF-8/malformed
  JSON, unknown events/protocol, inconsistent or duplicate IDs or unmatched
  test starts/ends, no visible executed tests, or all tests skipped. The
  same declaration and artifact placement/freshness/size rules apply.

At exit 0, declaration, artifact and stream validity take precedence: a
malformed or truncated stream is UNKNOWN even if an earlier event recorded
a failure. Only a complete supported stream receives an event-based verdict.
The reader scans through the final event; a passing prefix is insufficient.
Exactly one `allSuites` event must match the number of suites seen.
Successful hidden tests that load suites or run setup never count as passed
or executed tests. Skipped visible tests are in `total` and `notExecuted`,
but not `executed`. Counts are derived from unique matched test IDs, so
parallel suites do not double-count, and a late error removes an earlier
success from the passed count. An error makes a formerly hidden test visible
in the counters. Any error event still means FAIL if a later retry succeeds.

A passing result is reported as a `passed` check in the turn's host observation; the host's source, ownership and overlap checks still apply. It earns no test credit by itself: credit comes only from registered verification ([test evidence](engram-host-adapter.md#test-evidence)). A declaration by itself grants no credit. PASS is never inferred from exit 0 alone or from console output. UNKNOWN is not a weaker PASS: it earns nothing and names what was observed.

## Captured .NET result shapes

The committed [capture matrix](../../src/tests/fixtures/declared-trx/runner-shapes/README.md)
covers these exact Windows x64 producers with .NET SDK 9.0.318 and net9.0:

| Framework and report producer | Passing run | Failing run | Zero-test run |
|---|---|---|---|
| NUnit 4.4.0, NUnit3TestAdapter 5.0.0, VSTest 17.14.1 TRX | PASS | FAIL | UNKNOWN |
| MSTest 4.1.0, VSTest 17.14.1 TRX | PASS | FAIL | UNKNOWN |
| MSTest.Sdk 4.1.0, MTP 2.1.0, TrxReport 2.1.0 | PASS | FAIL | FAIL |

The zero-test difference is the producer's own verdict. In these captures,
VSTest returns exit 0 with `outcome="Completed"` and `executed=0`, which is
UNKNOWN. MTP returns native exit 8 (reported as exit 1 by `dotnet test`),
with `outcome="Failed"`, a run-level error and zero counters. It is FAIL by
both the nonzero-exit and explicit-failure rules. TermAl does not turn that
failed run into UNKNOWN or PASS. No reader exception is needed.

MTP requires its TRX reporting extension; `--logger trx` is the VSTest
option. The captured SDK-9 MTP project passes extension arguments after
`--`, using `--report-trx --report-trx-filename results.trx`. The capture
README contains the pinned project files and exact commands. These are
finite producer examples, not a certification of other versions or
frameworks. Command matching and artifact rules remain the same.

## Examples

.NET under VSTest (`dotnet test`), one project and one target framework per entry (a fixed report name across several target frameworks overwrites; the Testing Platform writes TRX through its reporting extension with `--report-trx`, a different command):

```toml
[[test]]
command  = "dotnet test tests/Orders.Tests --logger \"trx;LogFileName=results.trx\" --results-directory tests/Orders.Tests/.termal-results"
cwd      = "."
artifact = "tests/Orders.Tests/.termal-results/results.trx"
```

One project and one target framework per entry: a fixed `LogFileName` across several target frameworks overwrites the report. This exact line is recognised only by a running host that implements declared commands.

Proposed JUnit example for a future reader. A matching command can be recorded as a declared check, but the current readers cannot parse its JUnit results or report them as passed; at exit 0, the result is UNKNOWN. A project-provided wrapper runs the real tests synchronously, propagates their exit status, and writes exactly that one complete JUnit file (direct `npx vitest` and `pytest` are built-in checks and never take the declared route):

```toml
[[test]]
command  = "node scripts/project-tests.mjs"
cwd      = "."
artifact = ".termal-results/junit.xml"
```

`.gitignore` in both cases: `.termal-results/`.

Flutter main suite (including the Windows SDK's `flutter.bat` reached through
`flutter` on PATH); use this exact command in the one-call form:

```toml
[[test]]
command  = "flutter test --no-pub --exclude-tags supabase-local --file-reporter json:.termal-results/flutter.json"
cwd      = "."
artifact = ".termal-results/flutter.json"
```

Pure Dart package in a subdirectory of the same repository:

```toml
[[test]]
command  = "dart test --file-reporter json:.termal-results/dart.json"
cwd      = "packages/common_core"
artifact = "packages/common_core/.termal-results/dart.json"
```

Ignore `.termal-results/` in each package. Reporter paths are relative to the
command's cwd; `artifact` and `cwd` in the declaration are relative to the
repository root. A separate Flutter package uses its own entry and cwd in
the same way. Prepare dependencies first when using `--no-pub`. A run with
`--name`, `--plain-name`, `--tags`, a file/directory filter, or an added
`--reporter expanded` needs its own exact declaration. Lint/format/analyze
commands are not test commands. This route never starts external services;
required environment values must be configured separately on the executing host.

## Workflow

Operator, once per repository: write `termal-tests.toml`, ignore the result directory, commit both, run one declared command once and check its outcome. Internal host diagnostics emit one bounded `declared-check` line at declared completion, with format, verdict, numeric counters (or `unavailable`) and a fixed reason code such as `artifact_missing`, `artifact_not_ignored` or `dart_missing_done`. They contain no artifact path, command, test name or test output. Counts and reason codes are internal log diagnostics, not new public check fields.

Agent, each run: read `termal-tests.toml` at the root; run one entry exactly as declared, from its `cwd`, in the recognised one-call form; then read the check outcome. A passing result is reported as passed, and is not test credit. For an indeterminate/UNKNOWN result, inspect the internal host diagnostic with the operator. Its reason is an observation, not a diagnosed cause: on `artifact_missing`, check that the logger flag and result path match the declaration; on `artifact_not_ignored`, check the ignore rule; on `dart_no_executed_tests`, check what the command selects. Do not retype the command with other flags to make it match, and do not edit the declaration to fit a command already run: fix the declaration, commit it under the repository's rules, run again.

Snippet for a repository's own agent instructions:

"Test commands are declared in termal-tests.toml at the root. Run one exactly as declared, from its cwd, as a single call (`pushd "DIR" && <command>`). TermAl judges it from the result file it writes; a passing result is reported as passed but is not test credit, UNKNOWN is not PASS and names what was observed. This requires a running TermAl build that implements declared commands."

## What the pilot does not do

- It does not capture the environment of the agent's shell; TermAl cannot see it, and the artifact route does not need it.
- It does not parse console output on the declared route; a declared run without an artifact is never PASS: it is UNKNOWN, or FAIL on a non-zero exit. Built-in runners are unchanged.
- Its captured framework/version matrix is finite: readers accept the documented TRX shape or Dart JSON protocol above; a declaration is not a framework/version certification.
- It trusts the declaration as current-worktree configuration; it cannot tell a real test command from a script that writes a plausible file.
- It observes absence or the existing bounded file hash at command start and compares the completed artifact: correlation, not proof that the command produced it.
- It reads the declaration at command start and keeps only the matched entry and the file hash on the check; no declaration history or lifecycle.
- JUnit XML is not implemented in `2b1e3f85`; its outcome rules and example above describe a proposed follow-up.
- Nothing here changes which commands TermAl recognised before, nor any rule for commit, push, tracker or approval.
