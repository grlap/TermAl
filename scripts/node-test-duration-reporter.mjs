// Node test-runner reporter for the launcher's per-test duration report.
//
// Owns: turning the runner's structured events into one JSON line each, with
// the fields the duration report reads (test identity, outcome, duration and
// the per-file summary). It writes only to the destination the launcher names
// beside the human-readable reporter, so the stage log is unchanged.
// Does not own: thresholds, ranking or the launcher summary, which live in
// `test-durations.mjs`; it never decides whether a stage passed.
const recorded = new Set(["test:start", "test:pass", "test:fail", "test:summary"]);

function line(event) {
  const data = event.data ?? {};
  const details = data.details ?? {};
  const error = details.error;
  return `${JSON.stringify({
    type: event.type,
    name: typeof data.name === "string" ? data.name : null,
    nesting: Number.isInteger(data.nesting) ? data.nesting : null,
    file: typeof data.file === "string" ? data.file : null,
    kind: typeof details.type === "string" ? details.type : null,
    durationMs: Number.isFinite(details.duration_ms)
      ? details.duration_ms
      : Number.isFinite(data.duration_ms) ? data.duration_ms : null,
    skipped: data.skip !== undefined && data.skip !== false,
    todo: data.todo !== undefined && data.todo !== false,
    timedOut: error?.failureType === "testTimeoutFailure",
  })}\n`;
}

export default async function* durationReporter(source) {
  for await (const event of source) {
    if (recorded.has(event.type)) yield line(event);
  }
}
