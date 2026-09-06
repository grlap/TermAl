import { describe, expect, it, vi } from "vitest";

import {
  addSessionHistoryPageDemandListener,
  completeSessionHistoryPageDemand,
  deferSessionHistoryTailDemandUntilStateAdoption,
  requestSessionHistoryAroundPage,
  requestSessionHistoryOlderPage,
  requestSessionHistoryPage,
  requestSessionHistoryStartPage,
  requestSessionHistoryTailPage,
  resolveHasOlderSessionHistory,
  resumeSessionHistoryDemandsAfterStateAdoption,
} from "./session-history-demand";

describe("resolveHasOlderSessionHistory", () => {
  it("uses only explicit history availability, never inferred hydration or counts", () => {
    expect(
      resolveHasOlderSessionHistory({
        hasOlderHistory: false,
      }),
    ).toBe(false);
    expect(
      resolveHasOlderSessionHistory({
        hasOlderHistory: true,
      }),
    ).toBe(true);
    expect(resolveHasOlderSessionHistory({})).toBe(false);
    // An explicit undefined current field satisfies TypeScript's weak-type check.
    const retiredShape = {
      messageCount: 5_000,
      messagesLoaded: false,
      residentMessageCount: 20,
      hasOlderHistory: undefined,
    };
    expect(resolveHasOlderSessionHistory(retiredShape)).toBe(false);
  });
});

describe("session history page demand bridge", () => {
  it("retains one completable tail intent and resumes it only on state adoption", async () => {
    const listener = vi.fn();
    const remove = addSessionHistoryPageDemandListener(listener);
    try {
      const settled = vi.fn();
      const applied = requestSessionHistoryTailPage("recovering", { retryOnRecovery: true });
      void applied.then(settled);
      const demand = listener.mock.calls[0]![0];
      expect(deferSessionHistoryTailDemandUntilStateAdoption(demand)).toBe(true);
      await Promise.resolve();
      expect(settled).not.toHaveBeenCalled();
      expect(listener).toHaveBeenCalledOnce();
      resumeSessionHistoryDemandsAfterStateAdoption();
      expect(listener).toHaveBeenCalledTimes(2);
      expect(listener.mock.calls[1]![0]).toBe(demand);
      // An adoption while a fetch is active must not start another fetch.
      resumeSessionHistoryDemandsAfterStateAdoption();
      expect(listener).toHaveBeenCalledTimes(2);
      completeSessionHistoryPageDemand(demand.requestId, true);
      await expect(applied).resolves.toBe(true);
      resumeSessionHistoryDemandsAfterStateAdoption();
      expect(listener).toHaveBeenCalledTimes(2);
    } finally {
      remove();
    }
  });

  it("bounds recovery by two rejection/adoption cycles, not elapsed time", async () => {
    const listener = vi.fn();
    const remove = addSessionHistoryPageDemandListener(listener);
    try {
      const applied = requestSessionHistoryTailPage("exhausted", { retryOnRecovery: true });
      const demand = listener.mock.calls[0]![0];
      for (let cycle = 0; cycle < 2; cycle += 1) {
        expect(deferSessionHistoryTailDemandUntilStateAdoption(demand)).toBe(true);
        // Duplicate rejection while already waiting consumes no extra attempt.
        expect(deferSessionHistoryTailDemandUntilStateAdoption(demand)).toBe(true);
        resumeSessionHistoryDemandsAfterStateAdoption();
      }
      expect(deferSessionHistoryTailDemandUntilStateAdoption(demand)).toBe(false);
      completeSessionHistoryPageDemand(demand.requestId, false);
      await expect(applied).resolves.toBe(false);
      resumeSessionHistoryDemandsAfterStateAdoption();
      expect(listener).toHaveBeenCalledTimes(3);
    } finally {
      remove();
    }
  });

  it.each(["abort", "owner-unmount"] as const)(
    "forgets a recovery waiter after %s and never replays it to a later owner",
    async (cause) => {
      const controller = new AbortController();
      const listener = vi.fn();
      const remove = addSessionHistoryPageDemandListener(listener);
      const applied = requestSessionHistoryTailPage("cancelled", {
        retryOnRecovery: true, signal: controller.signal,
      });
      expect(deferSessionHistoryTailDemandUntilStateAdoption(listener.mock.calls[0]![0])).toBe(true);
      if (cause === "abort") controller.abort();
      remove();
      await expect(applied).resolves.toBe(false);
      const later = vi.fn();
      const removeLater = addSessionHistoryPageDemandListener(later);
      try {
        resumeSessionHistoryDemandsAfterStateAdoption();
        expect(later).not.toHaveBeenCalled();
      } finally {
        removeLater();
      }
    },
  );

  it("does not dispatch a tail request whose signal was already aborted", async () => {
    const listener = vi.fn();
    const remove = addSessionHistoryPageDemandListener(listener);
    const controller = new AbortController();
    controller.abort();
    try {
      await expect(requestSessionHistoryTailPage("cancelled-before-send", {
        signal: controller.signal, retryOnRecovery: true,
      })).resolves.toBe(false);
      expect(listener).not.toHaveBeenCalled();
    } finally {
      remove();
    }
  });

  it("leaves non-opted-in tail and other navigation requests terminal", async () => {
    const listener = vi.fn();
    const remove = addSessionHistoryPageDemandListener(listener);
    try {
      const requests = [
        requestSessionHistoryTailPage("ordinary"),
        requestSessionHistoryStartPage("ordinary"),
      ];
      for (const [demand] of listener.mock.calls) {
        expect(deferSessionHistoryTailDemandUntilStateAdoption(demand)).toBe(false);
        completeSessionHistoryPageDemand(demand.requestId, false);
      }
      await expect(Promise.all(requests)).resolves.toEqual([false, false]);
      resumeSessionHistoryDemandsAfterStateAdoption();
      expect(listener).toHaveBeenCalledTimes(2);
    } finally {
      remove();
    }
  });

  it("fails completable demand immediately when no owner is mounted", async () => {
    await expect(
      requestSessionHistoryStartPage("session-unmounted"),
    ).resolves.toBe(false);
  });

  it("replays demand emitted before a listener is registered", () => {
    requestSessionHistoryPage("session-1");

    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-1",
      direction: "older",
    });

    removeListener();
  });

  it("dedupes pending demand for the same session", () => {
    requestSessionHistoryPage("session-2");
    requestSessionHistoryPage("session-2");

    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-2",
      direction: "older",
    });

    removeListener();
  });

  it("resolves bounded start-page demand after the listener applies it", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryStartPage("session-start");
    const demand = listener.mock.calls[0]?.[0];
    expect(demand).toMatchObject({
      sessionId: "session-start",
      direction: "start",
    });

    completeSessionHistoryPageDemand(demand?.requestId, true);
    await expect(applied).resolves.toBe(true);

    removeListener();
  });

  it("emits a completable older-page demand for prompt navigation", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryOlderPage("session-older");
    const demand = listener.mock.calls[0]?.[0];
    expect(demand).toMatchObject({
      sessionId: "session-older",
      direction: "older",
    });

    completeSessionHistoryPageDemand(demand?.requestId, true);
    await expect(applied).resolves.toBe(true);

    removeListener();
  });

  it("emits a completable live-tail reattachment demand", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryTailPage("session-history");
    const demand = listener.mock.calls[0]?.[0];
    expect(demand).toMatchObject({
      sessionId: "session-history",
      direction: "tail",
    });

    completeSessionHistoryPageDemand(demand?.requestId, true);
    await expect(applied).resolves.toBe(true);

    removeListener();
  });

  it("emits a completable around-position demand", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryAroundPage("session-around", 420);
    const demand = listener.mock.calls[0]?.[0];
    expect(demand).toMatchObject({
      sessionId: "session-around",
      direction: "around",
      position: 420,
    });

    completeSessionHistoryPageDemand(demand?.requestId, true);
    await expect(applied).resolves.toBe(true);

    removeListener();
  });

  it("settles accepted demand when its owner unmounts before completion", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryTailPage("session-unmounted-late");
    expect(listener).toHaveBeenCalledTimes(1);

    removeListener();
    await expect(applied).resolves.toBe(false);
  });
});
