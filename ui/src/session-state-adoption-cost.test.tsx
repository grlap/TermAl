// Owns isolated measurements of the existing summary-adoption pipeline.
// Does not model browser layout, input scheduling, or live-host INP.
// Uses the shared App fixtures and the production transcript authority.
import { afterEach, describe, expect, it, vi } from "vitest";
import { makeSession, makeStateSessionSummary } from "./app-test-harness";
import { TranscriptRepairAuthority } from "./transcript-repair-authority";
import { reconcileStateSessionSummaries } from "./session-reconcile";
import { resetSessionStoreForTesting } from "./session-store";
import type { StateSessionSummary } from "./types";

class MeasuredAuthority extends TranscriptRepairAuthority {
  commitMilliseconds = 0;

  override commit(...args: Parameters<TranscriptRepairAuthority["commit"]>) {
    const start = performance.now();
    try {
      return super.commit(...args);
    } finally {
      this.commitMilliseconds += performance.now() - start;
    }
  }
}

function fixture(count: number, knownStamp: boolean) {
  const sessions = Array.from({ length: count }, (_, index) =>
    makeSession(`session-${index}`, {
      projectId: "project-local",
      messageCount: 0,
      queuePaused: false,
      sessionMutationStamp: knownStamp ? index + 1 : null,
      messagesLoaded: false,
      messageStartIndex: 0,
      hasOlderHistory: false,
      hasNewerHistory: false,
      promptHistory: [],
      promptDeliveries: [],
    }),
  );
  const summaries = sessions.map(makeStateSessionSummary);
  return { sessions, summaries };
}

describe("summary adoption work at reported session scale", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    resetSessionStoreForTesting();
  });

  it.each([
    [200, true],
    [2000, true],
    [8327, true],
    [8327, false],
  ] as const)("records %i sessions with known stamps=%s", (count, knownStamp) => {
    const { sessions, summaries } = fixture(count, knownStamp);
    const owner = new MeasuredAuthority({
      publish: () => {},
      drafts: () => ({}),
      attachments: () => ({}),
    }, sessions);
    owner.setServerInstance("fixture-instance");
    owner.adoptSummaries(summaries);
    const samples: object[] = [];
    // Three fixed samples describe variance; they are not retries or a gate.
    for (let sample = 0; sample < 3; sample += 1) {
      const before = owner.sessionsRef.current;
      const next = summaries.map((summary, index) => index === 0
        ? { ...summary, preview: `sample-${sample}`,
            sessionMutationStamp: knownStamp ? count + sample + 1 : null }
        : summary);
      const wire = JSON.stringify(next);
      const parseStart = performance.now();
      const parsed = JSON.parse(wire) as StateSessionSummary[];
      const parseMilliseconds = performance.now() - parseStart;

      const reconcileStart = performance.now();
      const reconciled = reconcileStateSessionSummaries(before, parsed);
      const reconcileMilliseconds = performance.now() - reconcileStart;

      owner.commitMilliseconds = 0;
      const adoptionStart = performance.now();
      const adopted = owner.adoptSummaries(parsed);
      const adoptionMilliseconds = performance.now() - adoptionStart;
      expect(adopted[0].preview).toBe(`sample-${sample}`);
      expect(reconciled[0].preview).toBe(`sample-${sample}`);
      expect(adopted.slice(1).every((session, index) => session === before[index + 1])).toBe(true);
      expect(adopted[0].messages).toBe(before[0].messages);
      samples.push({ parseMilliseconds, reconcileMilliseconds,
        adoptionMilliseconds, commitMilliseconds: owner.commitMilliseconds,
        wireBytes: new TextEncoder().encode(wire).length,
        changedSessionCount: adopted.filter((session, index) => session !== before[index]).length });
    }

    // Prove which suspected operation this actual summary path executes.
    const stringify = vi.spyOn(JSON, "stringify");
    const beforeNoop = owner.sessionsRef.current;
    owner.adoptSummaries(summaries);
    const stringifyCalls = stringify.mock.calls.length;
    stringify.mockRestore();
    expect(stringifyCalls).toBe(0);
    expect(owner.sessionsRef.current[beforeNoop.length - 1]).toBe(beforeNoop[beforeNoop.length - 1]);
    console.info("UI_ADOPTION_BASELINE", JSON.stringify({ count, knownStamp,
      samples, stringifyCalls,
      limitations: "Synthetic session metadata; owner pipeline only. No browser input delay, layout, paint, live payload or event-frequency claim." }));
  });
});
