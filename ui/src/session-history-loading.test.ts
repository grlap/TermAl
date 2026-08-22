import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import * as sessionHistoryDemand from "./session-history-demand";
import {
  loadBoundedSessionHistoryWindow,
  loadOlderHistoryPageOnce,
  type SessionHistoryLoadingContext,
} from "./session-history-loading";
import type { Message, Session } from "./types";

vi.mock("./session-history-demand", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./session-history-demand")>();
  return {
    ...actual,
    completeSessionHistoryPageDemand: vi.fn(),
  };
});

type TextMessage = Extract<Message, { type: "text" }>;

function message(index: number): TextMessage {
  return {
    type: "text",
    id: `message-${index}`,
    timestamp: "12:00",
    author: "assistant",
    text: `Message ${index}`,
  };
}

function session(messages: Message[], options: Partial<Session> = {}): Session {
  return {
    id: "session-1",
    name: "History",
    emoji: "H",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt",
    status: "idle",
    preview: "",
    messages,
    messageCount: messages.length,
    messagesLoaded: false,
    sessionMutationStamp: 1,
    ...options,
  };
}

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

function expectSingleDemandCompletion(requestId: number, applied: boolean) {
  expect(
    sessionHistoryDemand.completeSessionHistoryPageDemand,
  ).toHaveBeenCalledOnce();
  expect(
    sessionHistoryDemand.completeSessionHistoryPageDemand,
  ).toHaveBeenCalledWith(requestId, applied);
}

async function expectPreFetchDemandExit({
  context,
  direction,
  requestId,
}: {
  context: SessionHistoryLoadingContext;
  direction: "older" | "newer" | "tail";
  requestId: number;
}) {
  const fetchSessionHistory = vi
    .spyOn(api, "fetchSessionHistory")
    .mockRejectedValue(new Error("unexpected pre-fetch guard regression"));

  await loadBoundedSessionHistoryWindow({
    context,
    demand: {
      direction,
      requestId,
      sessionId: "session-1",
    },
  });

  expect(fetchSessionHistory).not.toHaveBeenCalled();
  expectSingleDemandCompletion(requestId, false);
}

describe("session history loading", () => {
  afterEach(() => {
    vi.clearAllMocks();
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

  it("completes an applied bounded demand exactly once", async () => {
    const current = session([message(1)]);
    vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
      hasMore: false,
      messageCount: 1,
      messages: [message(0)],
      revision: 1,
      serverInstanceId: "server-a",
      sessionMutationStamp: 1,
    });
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);
    vi.mocked(context.publishSession).mockReturnValue(true);

    await loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "start",
        requestId: 101,
        sessionId: current.id,
      },
    });

    expect(context.publishSession).toHaveBeenCalledOnce();
    expectSingleDemandCompletion(101, true);
  });

  it("completes a merge-rejected bounded demand exactly once", async () => {
    const current = session([message(0), message(1)], {
      messageCount: 2,
    });
    vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
      // A shorter tail with the same mutation stamp produces metadataChanged.
      hasMore: false,
      messageCount: 1,
      messages: [message(0)],
      revision: 1,
      serverInstanceId: "server-a",
      sessionMutationStamp: 1,
    });
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);

    await loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "tail",
        requestId: 102,
        sessionId: current.id,
      },
    });

    expect(context.requestActionRecoveryResync).toHaveBeenCalledOnce();
    expect(context.publishSession).not.toHaveBeenCalled();
    expectSingleDemandCompletion(102, false);
  });

  it("completes a failed bounded demand exactly once", async () => {
    const requestError = new Error("history unavailable");
    vi.spyOn(api, "fetchSessionHistory").mockRejectedValue(requestError);
    const current = session([message(0)]);
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);

    await loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "tail",
        requestId: 103,
        sessionId: current.id,
      },
    });

    expect(context.reportRequestError).toHaveBeenCalledOnce();
    expect(context.reportRequestError).toHaveBeenCalledWith(requestError);
    expectSingleDemandCompletion(103, false);
  });

  it("settles silently when its owner unmounts during a bounded fetch", async () => {
    const historyPage = deferred<
      Awaited<ReturnType<typeof api.fetchSessionHistory>>
    >();
    vi.spyOn(api, "fetchSessionHistory").mockImplementation(
      () => historyPage.promise,
    );
    const current = session([message(0)]);
    let mounted = true;
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);
    context.isMounted = () => mounted;

    const load = loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "tail",
        requestId: 104,
        sessionId: current.id,
      },
    });
    mounted = false;
    historyPage.resolve({
      hasMore: false,
      messageCount: 1,
      messages: [message(0)],
      revision: 1,
      serverInstanceId: "server-a",
      sessionMutationStamp: 1,
    });
    await load;

    expect(context.publishSession).not.toHaveBeenCalled();
    expect(context.reportRequestError).not.toHaveBeenCalled();
    expect(context.requestActionRecoveryResync).not.toHaveBeenCalled();
    expectSingleDemandCompletion(104, false);
  });

  it("settles silently when a bounded fetch rejects after owner unmount", async () => {
    const historyPage = deferred<
      Awaited<ReturnType<typeof api.fetchSessionHistory>>
    >();
    vi.spyOn(api, "fetchSessionHistory").mockImplementation(
      () => historyPage.promise,
    );
    const current = session([message(0)]);
    let mounted = true;
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);
    context.isMounted = () => mounted;

    const load = loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "tail",
        requestId: 112,
        sessionId: current.id,
      },
    });
    mounted = false;
    historyPage.reject(new Error("history unavailable after unmount"));
    await load;

    expect(context.publishSession).not.toHaveBeenCalled();
    expect(context.reportRequestError).not.toHaveBeenCalled();
    expect(context.requestActionRecoveryResync).not.toHaveBeenCalled();
    expectSingleDemandCompletion(112, false);
  });

  it("completes an instance-mismatched bounded demand exactly once", async () => {
    const current = session([message(0)]);
    vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
      hasMore: false,
      messageCount: 1,
      messages: [message(0)],
      revision: 1,
      serverInstanceId: "server-b",
      sessionMutationStamp: 1,
    });
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);

    await loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "tail",
        requestId: 105,
        sessionId: current.id,
      },
    });

    expect(context.requestActionRecoveryResync).toHaveBeenCalledOnce();
    expect(context.requestActionRecoveryResync).toHaveBeenCalledWith({
      allowUnknownServerInstance: true,
    });
    expect(context.publishSession).not.toHaveBeenCalled();
    expect(context.reportRequestError).not.toHaveBeenCalled();
    expectSingleDemandCompletion(105, false);
  });

  it("maps an applied older-page load onto successful demand completion", async () => {
    const current = session([message(1)], {
      messageCount: 2,
      messageStartIndex: 1,
    });
    vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
      hasMore: false,
      messageCount: 2,
      messages: [message(0)],
      revision: 1,
      serverInstanceId: "server-a",
      sessionMutationStamp: 1,
    });
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);
    vi.mocked(context.publishSession).mockReturnValue(true);

    await loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "older",
        requestId: 106,
        sessionId: current.id,
      },
    });

    expect(api.fetchSessionHistory).toHaveBeenCalledWith(current.id, {
      before: "message-1",
      limit: expect.any(Number),
    });
    expect(context.publishSession).toHaveBeenCalledOnce();
    expectSingleDemandCompletion(106, true);
  });

  it("loads and publishes a bounded window around the requested position", async () => {
    const centeredMessages = Array.from({ length: 20 }, (_, index) =>
      message(index + 40),
    );
    const current = session([message(900), message(901)], {
      messageCount: 1_000,
      messageStartIndex: 900,
    });
    vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
      hasMore: true,
      hasNewer: true,
      messageCount: 1_000,
      messages: centeredMessages,
      messageStartIndex: 40,
      nextAfter: "message-59",
      nextBefore: "message-40",
      revision: 1,
      serverInstanceId: "server-a",
      sessionMutationStamp: 1,
    });
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);
    vi.mocked(context.publishSession).mockReturnValue(true);

    await loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "around",
        position: 50,
        requestId: 113,
        sessionId: current.id,
      },
    });

    expect(api.fetchSessionHistory).toHaveBeenCalledWith(current.id, {
      around: 50,
      limit: expect.any(Number),
    });
    expect(context.publishSession).toHaveBeenCalledWith(
      expect.objectContaining({
        messageStartIndex: 40,
        messages: centeredMessages,
      }),
    );
    expectSingleDemandCompletion(113, true);
  });

  it("defaults a missing around position to the transcript start", async () => {
    const current = session([message(0)]);
    vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
      hasMore: false,
      hasNewer: true,
      messageCount: 2,
      messages: [message(0)],
      messageStartIndex: 0,
      nextAfter: "message-0",
      revision: 1,
      serverInstanceId: "server-a",
      sessionMutationStamp: 1,
    });
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);
    vi.mocked(context.publishSession).mockReturnValue(true);

    await loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "around",
        requestId: 114,
        sessionId: current.id,
      },
    });

    expect(api.fetchSessionHistory).toHaveBeenCalledWith(current.id, {
      around: 0,
      limit: expect.any(Number),
    });
    expect(context.publishSession).toHaveBeenCalledOnce();
    expectSingleDemandCompletion(114, true);
  });

  it("reports a protocol error and completes its demand exactly once", async () => {
    const current = session([message(0)]);
    vi.spyOn(api, "fetchSessionHistory").mockResolvedValue({
      hasMore: false,
      hasNewer: true,
      messageCount: 1,
      messages: [message(0)],
      nextAfter: "message-0",
      revision: 1,
      serverInstanceId: "server-a",
      sessionMutationStamp: 1,
    });
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(current);

    await loadBoundedSessionHistoryWindow({
      context,
      demand: {
        direction: "tail",
        requestId: 107,
        sessionId: current.id,
      },
    });

    expect(context.reportRequestError).toHaveBeenCalledOnce();
    expect(context.reportRequestError).toHaveBeenCalledWith(
      expect.objectContaining({
        message:
          "session history tail page did not end at the live transcript tail",
      }),
    );
    expect(context.publishSession).not.toHaveBeenCalled();
    expectSingleDemandCompletion(107, false);
  });

  it("completes before fetching when its owner is already unmounted", async () => {
    const context = makeContext();
    context.isMounted = () => false;

    await expectPreFetchDemandExit({
      context,
      direction: "tail",
      requestId: 108,
    });
  });

  it("completes before fetching when its session is missing", async () => {
    await expectPreFetchDemandExit({
      context: makeContext(),
      direction: "tail",
      requestId: 109,
    });
  });

  it("completes before fetching when a newer cursor is missing", async () => {
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(session([]));

    await expectPreFetchDemandExit({
      context,
      direction: "newer",
      requestId: 110,
    });
  });

  it("completes before fetching when an older cursor is missing", async () => {
    const context = makeContext();
    vi.mocked(context.getSession).mockReturnValue(session([]));

    await expectPreFetchDemandExit({
      context,
      direction: "older",
      requestId: 111,
    });
  });
});
