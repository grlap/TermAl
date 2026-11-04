# Captured .NET TRX shapes

These nine files were produced by real, single-project Windows x64 runs on
2026-11-03 with .NET SDK 9.0.318, net9.0 and runtime 9.0.20. They extend the
existing xUnit/VSTest examples with a finite NUnit, MSTest and MTP matrix.
They are reports, not hand-authored substitutes for runner output.

| Directory | Framework / adapter | Report producer | Pass | Fail | Zero |
|---|---|---|---|---|---|
| `nunit` | NUnit 4.4.0 / NUnit3TestAdapter 5.0.0 | VSTest 17.14.1 | PASS | FAIL | UNKNOWN |
| `mstest` | MSTest framework/adapter 4.1.0 | VSTest 17.14.1 | PASS | FAIL | UNKNOWN |
| `mtp` | MSTest.Sdk 4.1.0 / MSTest 4.1.0 | MTP 2.1.0 / TrxReport 2.1.0 | PASS | FAIL | FAIL |

`captures.json` records each exact command, process exit, expected verdict,
run outcome, all counters, original SHA-256 and sanitized SHA-256.

## Why the two empty-run verdicts differ

Both VSTest producers exit 0 and write `Completed` with zero executed tests:
the reader returns UNKNOWN, with reason `no tests executed`.

The MTP producer writes `Failed`, zero counters and a run-level `Error`
stating native exit code 8. Its `dotnet test` MSBuild wrapper exits 1.
The explicit failed outcome and nonzero exit each require FAIL. The host
does not override the producer's failed run because its test counters are
zero. No reader change or special-case fallback is needed.

These are the captured default producer policies. They do not certify
other package versions, platforms, option combinations or report formats.

## Reproduce

Copy `projects/` into a fresh Git-ignored scratch directory under the
repository's `.tmp/`; do not run builds in the committed fixture directory.
Keep the copied directory layout so `global.json` pins SDK 9.0.318.
NuGet restores the versions recorded in the projects. MTP's SDK profile is
`None` with only the TRX reporting extension explicitly enabled.

Run each case once from the copied `nunit/` and `mstest/` directories.
Replace `CASE` with `pass`, `fail` or `zero`:

```text
dotnet test -nodeReuse:false -maxcpucount:1 -p:UseSharedCompilation=false -p:CaptureCase=CASE --logger "trx;LogFileName=CASE.trx" --results-directory .termal-results
```

From the copied `mtp/` directory use the MTP reporting options after
`--`, as supported by this SDK-9 integration:

```text
dotnet test -nodeReuse:false -maxcpucount:1 -p:UseSharedCompilation=false -p:CaptureCase=CASE -- --report-trx --report-trx-filename CASE.trx --results-directory .termal-results
```

`CaptureCase=pass` compiles one passing test, `fail` one intentionally
failing assertion, and `zero` no test methods. Every command builds its own
selected case; none uses `--no-build`, result reuse or a fabricated report.
The constant passing MSTest assertion emits the expected MSTEST0032 analyzer
warning; that warning does not change the captured TRX counters.
The failure cases are intentional capture inputs, not failed product gates.

The report is `.termal-results/CASE.trx`. MTP also writes a per-module log;
its zero-test log identifies the native exit 8 behind the wrapper's exit 1.

## Sanitizing

Only machine-specific identity/path values were replaced: the root run
name, run user, computer name and deployment directory have fixed capture
values; absolute paths below the source capture checkout now start with
`C:\capture`; user/computer tokens in producer diagnostic text are
anonymized. An optional UTF-8 BOM is removed and line endings are normalized
to LF; the sanitized hashes describe those canonical fixture bytes. Report
structure, IDs, timestamps, outcomes, counters, test payloads and failure/exit
words are otherwise preserved. The source and sanitized ResultSummary/Counters prefix was
compared byte-for-byte before each fixture was written.

## Producer references

- [NUnit test adapter](https://docs.nunit.org/articles/vs-test-adapter/Index.html)
- [MSTest SDK runner and profiles](https://learn.microsoft.com/en-us/dotnet/core/testing/unit-testing-mstest-sdk)
- [MTP TRX reporting extension](https://learn.microsoft.com/en-us/dotnet/core/testing/microsoft-testing-platform-test-reports)

These documents explain command/report setup. The committed captures,
not documentation examples, establish the finite result-shape expectations.
