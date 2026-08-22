import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import {
  loadOlderHistoryPageOnce,
  type SessionHistoryLoadingContext,
} from "./session-history-loading";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

function makeContext(): SessionHistoryLoadingContext {
  return {
    getLastSeenServerInstanceId: () => "server-a",
    getSession: vi.fn(),
    inFlightOlderLoads: new Map(),
    isMounted: () => true,
    publishSession: vi.fn(),
    reportRequestError: vi.fn(),
    requestActionRecoveryResync: vi.fn(),
  };
}

describe("session history loading", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("reports one error to all waiters sharing a failed older-page load", async () => {
    const historyPage = deferred<
      Awaited<ReturnType<typeof api.fetchSessionHistory>>
    >();
    const fetchSessionHistory = vi
      .spyOn(api, "fetchSessionHistory")
      .mockImplementation(() => historyPage.promise);
    const context = makeContext();
    const first = loadOlderHistoryPageOnce({
      context,
      requestedBefore: "message-64",
      sessionId: "session-1",
    });
    const second = loadOlderHistoryPageOnce({
      context,
      requestedBefore: "message-64",
      sessionId: "session-1",
    });

    expect(second).toBe(first);
    const requestError = new Error("history unavailable");
    historyPage.reject(requestError);

    await expect(Promise.all([first, second])).resolves.toEqual([
      { kind: "failed" },
      { kind: "failed" },
    ]);
    expect(fetchSessionHistory).toHaveBeenCalledOnce();
    expect(context.reportRequestError).toHaveBeenCalledOnce();
    expect(context.reportRequestError).toHaveBeenCalledWith(requestError);
    expect(context.inFlightOlderLoads.size).toBe(0);
  });

  it("requests instance recovery without publishing a mismatched page", async () => {
    vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
      hasMore: false,
      messageCount: 1,
      messages: [],
      nextBefore: null,
      revision: 1,
      serverInstanceId: "server-b",
      sessionMutationStamp: 1,
    });
    const context = makeContext();

    await expect(
      loadOlderHistoryPageOnce({
        context,
        requestedBefore: "message-64",
        sessionId: "session-1",
      }),
    ).resolves.toEqual({ kind: "unavailable" });

    expect(context.requestActionRecoveryResync).toHaveBeenCalledWith({
      allowUnknownServerInstance: true,
    });
    expect(context.publishSession).not.toHaveBeenCalled();
    expect(context.reportRequestError).not.toHaveBeenCalled();
    expect(context.inFlightOlderLoads.size).toBe(0);
  });
});
