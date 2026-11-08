// Bounded internal diagnostics for the declared artifact owner's completion.
// Uses the already-read result; no artifact read, lifecycle or wire ownership.
// Only fixed labels/reason codes and numeric counters may reach the host log.

fn engram_write_declared_diagnostic(log: &mut impl std::io::Write, result: &EngramDeclaredResult) {
    let format = if result.artifact.is_none() { "unread" } else {
        match result.format {
            EngramDeclaredArtifactFormat::Trx => "trx",
            EngramDeclaredArtifactFormat::DartJson => "dart-json",
        }
    };
    let (verdict, reason) = match &result.verdict {
        EngramTrxVerdict::Pass => ("PASS", "none"),
        EngramTrxVerdict::Fail(reason) => ("FAIL", engram_declared_reason_code(reason)),
        EngramTrxVerdict::Unknown(reason) => ("UNKNOWN", engram_declared_reason_code(reason)),
    };
    let counters = result.counters.as_ref().map_or_else(|| "unavailable".to_owned(), |counts| {
        ["total", "executed", "passed", "failed", "error", "notExecuted"].iter()
            .map(|name| format!("{name}={}", counts.get(name))).collect::<Vec<_>>().join(",")
    });
    // A closed stderr pipe must not interrupt observation or change the verdict.
    let _ = writeln!(log, "declared-check format={format} verdict={verdict} counters={counters} reason={reason}");
}

fn engram_declared_reason_code(reason: &str) -> &'static str {
    match reason {
        "the declaration changed while the command ran" => "declaration_changed",
        "the command ended without an exit status" => "exit_unavailable",
        "the artifact's start inspection failed" => "artifact_start_unreadable",
        "the artifact is missing at the end" => "artifact_missing",
        "the artifact cannot be read" => "artifact_unreadable",
        "the artifact is outside the worktree" => "artifact_outside_root",
        "the artifact is not at the canonical path its start bound" => "artifact_path_changed",
        "the artifact is not a regular file" => "artifact_not_file",
        "the artifact is too large" => "artifact_too_large",
        "the artifact changed while it was read" => "artifact_changed_during_read",
        "the artifact is unchanged since the start" => "artifact_unchanged",
        "the artifact was modified before the command started" => "artifact_predates_start",
        "the artifact's ignore state cannot be read" => "artifact_ignore_unreadable",
        "the artifact is not ignored, or it is tracked" => "artifact_not_ignored",
        "the Dart JSON reporter stream is not UTF-8" => "dart_not_utf8",
        "the Dart JSON reporter has an event after done" => "dart_event_after_done",
        "the Dart JSON reporter has a malformed or unknown event" => "dart_invalid_event",
        "the Dart JSON reporter is missing its start event" => "dart_missing_start",
        "the Dart JSON reporter start or protocol version is unsupported" => "dart_unsupported_start",
        "the Dart JSON reporter repeats its suite count" => "dart_duplicate_suite_count",
        "the Dart JSON reporter repeats a suite id" => "dart_duplicate_suite",
        "the Dart JSON reporter has an inconsistent group" => "dart_invalid_group",
        "the Dart JSON reporter has an inconsistent testStart" => "dart_invalid_test_start",
        "the Dart JSON reporter testDone has no matching testStart" => "dart_unmatched_test_end",
        "the Dart JSON reporter has an inconsistent or unknown testDone" => "dart_invalid_test_end",
        "the Dart JSON reporter error has no matching testStart" => "dart_unmatched_error",
        "the Dart JSON reporter has an unsupported print event" => "dart_invalid_print",
        "the Dart JSON reporter debug event has no suite" => "dart_unmatched_debug",
        "the Dart JSON reporter has no completed done event" => "dart_missing_done",
        "the Dart JSON reporter stream is incomplete or inconsistent" => "dart_incomplete_stream",
        "the Dart JSON reporter records a failed test, error or run" => "dart_run_failed",
        "the Dart JSON reporter executed no non-hidden, non-skipped test" => "dart_no_executed_tests",
        "no tests executed" => "trx_no_executed_tests",
        "the TRX run passed no test" => "trx_no_passed_tests",
        "the TRX counters are inconsistent" => "trx_inconsistent_counters",
        "the TRX run records a run-level error" => "trx_run_error",
        "the TRX file is not UTF-8" => "trx_not_utf8",
        "the TRX file is not complete, well-formed XML" => "trx_invalid_xml",
        "the TRX file declares an encoding other than UTF-8" => "trx_encoding_unsupported",
        "the TRX file has a DTD" => "trx_dtd",
        "the artifact is not a TRX TestRun document" => "trx_wrong_document",
        "the TRX file has more than one ResultSummary" => "trx_duplicate_summary",
        "the TRX file has more than one Counters" => "trx_duplicate_counters",
        "the TRX Counters lack a counter real files carry" => "trx_missing_counter",
        _ if reason.starts_with("the command exited ") => "command_exit_nonzero",
        _ if reason.starts_with("environment_missing:") => "environment_missing",
        _ if reason.starts_with("the TRX run outcome is ") => "trx_run_outcome",
        _ if reason.starts_with("the TRX counter ") => "trx_failure_counter",
        _ if reason.starts_with("a TRX counter TermAl does not interpret ") => "trx_unknown_counter",
        _ => "invalid_result",
    }
}
