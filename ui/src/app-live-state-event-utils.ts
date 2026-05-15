// Owns lightweight state-event profiling and raw JSON metadata peeking.
// Does not own EventSource lifecycle, adoption, or reconnect state.
// Split from ui/src/app-live-state.ts.

const SLOW_STATE_EVENT_WARNING_MS = 50;
const STATE_EVENT_METADATA_PEEK_CHARS = 4096;

export function createStateEventProfiler() {
  if (
    !import.meta.env.DEV ||
    typeof performance === "undefined" ||
    typeof console === "undefined"
  ) {
    return null;
  }

  const startedAt = performance.now();
  let lastMarkAt = startedAt;
  const steps: string[] = [];

  return {
    mark(label: string) {
      const now = performance.now();
      steps.push(`${label}=${(now - lastMarkAt).toFixed(1)}ms`);
      lastMarkAt = now;
    },
    finish(details: {
      adopted?: boolean;
      revision?: number;
      sessionCount?: number;
    }) {
      const now = performance.now();
      const totalMs = now - startedAt;
      if (totalMs < SLOW_STATE_EVENT_WARNING_MS) {
        return;
      }

      const suffix = [
        `total=${totalMs.toFixed(1)}ms`,
        details.revision !== undefined ? `revision=${details.revision}` : null,
        details.adopted !== undefined ? `adopted=${details.adopted}` : null,
        details.sessionCount !== undefined
          ? `sessions=${details.sessionCount}`
          : null,
        ...steps,
      ]
        .filter(Boolean)
        .join(" ");
      console.warn(`[TermAl perf] slow state event ${suffix}`);
    },
  };
}

export function extractTopLevelJsonNumber(payload: string, key: string) {
  const match = new RegExp(`"${key}"\\s*:\\s*(-?\\d+)`).exec(
    payload.slice(0, STATE_EVENT_METADATA_PEEK_CHARS),
  );
  if (!match?.[1]) {
    return null;
  }

  const value = Number(match[1]);
  return Number.isFinite(value) ? value : null;
}

export function extractTopLevelJsonString(payload: string, key: string) {
  const match = new RegExp(
    `"${key}"\\s*:\\s*"([^"\\\\]*(?:\\\\.[^"\\\\]*)*)"`,
  ).exec(payload.slice(0, STATE_EVENT_METADATA_PEEK_CHARS));
  if (!match?.[1]) {
    return null;
  }

  try {
    return JSON.parse(`"${match[1]}"`) as string;
  } catch {
    return null;
  }
}

export function payloadHasTopLevelTrueBoolean(payload: string, key: string) {
  return new RegExp(`"${key}"\\s*:\\s*true(?:\\s*[,}])`).test(
    payload.slice(0, STATE_EVENT_METADATA_PEEK_CHARS),
  );
}
