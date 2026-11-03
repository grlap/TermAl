Real JSON file-reporter captures, 2026-11-03 on Windows x64:

- Dart SDK 3.13.2, package test 1.33.0 (test_api 0.7.15, test_core 0.6.21).
- Flutter 3.47.2 stable, framework d3b14c8769, Dart 3.13.2,
  flutter_test from that SDK (test_api 0.7.12).
- Both emit JSON reporter protocol 0.1.1. Flutter's runnerVersion is null.

Two disposable packages under the worktree's `.tmp/` used `test: any` for
pure Dart and `flutter_test: {sdk: flutter}` for Flutter. Each test imports
`package:test/test.dart` or `package:flutter_test/flutter_test.dart` and
defines one `test`: `expect(2 + 2, 4)` (pass), `expect(2 + 2, 5)` (fail),
or the passing body with `skip: true` (skipped).

The real commands, run from each package:

```text
dart test --file-reporter json:reports/pass.json test/pass_test.dart
dart test --file-reporter json:reports/fail.json test/fail_test.dart
dart test --file-reporter json:reports/skipped.json test/skipped_test.dart
flutter test --file-reporter json:reports/pass.json test/pass_test.dart
flutter test --file-reporter json:reports/fail.json test/fail_test.dart
flutter test --file-reporter json:reports/skipped.json test/skipped_test.dart
```

On this host these invoke `C:/github/flutter/bin/dart.bat` / `flutter.bat`.
Pass and all-skipped exited 0; fail exited 1. Only the captured JSON artifact
is an input to the Rust tests; console summaries are not parsed.

Sanitization replaces the worktree root with `/workspace` and process IDs
with zero. All event structure, IDs, flags, outcomes, timestamps and counters
remain as captured. Each `*-truncated.jsonl` is the passing capture's exact
sanitized prefix with the final `done` line removed, modeling interruption.
Hidden successful load tests appear in every file and must never count as
an executed visible test. The all-skipped file therefore has zero executed
tests despite its successful hidden load and `done.success=true`.

`flutter-main-suite.jsonl` is an additional real capture from the consumer's
main-suite command shape, with all three files loaded in one run:
`flutter test --no-pub --exclude-tags supabase-local --reporter expanded
--file-reporter json:reports/main-suite.json`. It exited 1 with one pass,
one fail and one skip, plus three hidden load tests. This pins counts when
events from multiple suites interleave.

`dart-print.jsonl` and `flutter-print.jsonl` are additional real captures
from the same packages and SDK versions. Their one passing test calls
`print('fixture output')` before the assertion:
`dart test --file-reporter json:reports/print.json test/print_test.dart`
and `flutter test --no-pub --file-reporter json:reports/print.json test/print_test.dart`.
Both exited 0 and emitted a `print` event. The same root/pid sanitization
applies; print messages and all event structure remain unchanged.
